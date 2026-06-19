//! Executor pipeline.
//!
//! For every PROCESSING bundle:
//!  1. Fetch the PP account from RPC (`getLedgerEntries`).
//!  2. Build an `InvokeContract` op against the bundle's channel contract, calling `transact`
//!     with the decoded MLXDR operations as `ScVal` args (one Bytes-encoded XDR per slot).
//!  3. Sign with the PP keypair (loaded from env once at boot).
//!  4. `sendTransaction` via Soroban RPC.
//!  5. Persist a `transactions` row + a `bundles_transactions` link by the returned hash.
//!
//! On any step failure: leave the bundle PROCESSING (or move to FAILED if retry budget exhausted).

use crate::auth::sep10::signing_key_from_seed;
use crate::config::Config;
use crate::events::{EventBroadcaster, ProviderEvent};
use crate::mlxdr;
use chrono::Duration as ChronoDuration;
use provider_stack_persistence::{
    BundleStatus, BundleTransactionRepo, OperationsBundleRepo, PgPool, TransactionRepo,
};
use soroban_client::contract::{ContractBehavior, Contracts};
use soroban_client::keypair::{Keypair, KeypairBehavior};
use soroban_client::network::{NetworkPassphrase, Networks};
use soroban_client::transaction::{
    assemble_transaction, TransactionBehavior, TransactionBuilder, TransactionBuilderBehavior,
};
use soroban_client::{Options, Server};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{interval, MissedTickBehavior};
use tracing::{debug, instrument, warn, Instrument};

#[instrument(skip_all, name = "pipeline.executor")]
pub async fn run(config: Arc<Config>, pool: PgPool, events: EventBroadcaster) {
    let server = match Server::new(
        &config.stellar_rpc_url,
        Options {
            allow_http: true,
            ..Options::default()
        },
    ) {
        Ok(s) => s,
        Err(e) => {
            warn!(error = ?e, "executor: Server::new failed; pipeline will not run");
            return;
        }
    };

    let mut tick = interval(config.mempool.executor_interval);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let processing = Arc::new(Mutex::new(false));

    loop {
        tick.tick().await;
        let mut guard = processing.lock().await;
        if *guard {
            continue;
        }
        *guard = true;
        drop(guard);

        let tick_span = tracing::info_span!("Executor.tick");
        if let Err(e) = run_tick(&server, &pool, &config, &events).instrument(tick_span).await {
            warn!(error = %e, "executor tick failed");
        }
        debug!("executor tick complete");

        let mut guard = processing.lock().await;
        *guard = false;
    }
}

/// One executor tick. Exposed for the integration test.
pub async fn run_tick(
    server: &Server,
    pool: &PgPool,
    config: &Config,
    events: &EventBroadcaster,
) -> anyhow::Result<()> {
    let bundles = OperationsBundleRepo::new(pool.clone());
    let txs = TransactionRepo::new(pool.clone());
    let link = BundleTransactionRepo::new(pool.clone());

    let kp = Keypair::from_secret(&config.pp_secret_key)
        .map_err(|e| anyhow::anyhow!("Keypair::from_secret failed: {e:?}"))?;
    let pubkey = kp.public_key();
    let passphrase = network_passphrase_for(&config.network);

    let processing = bundles
        .list_by_status(BundleStatus::Processing, config.mempool.slot_capacity as i64)
        .await?;

    let ctx = SubmitCtx {
        server,
        kp: &kp,
        pubkey: &pubkey,
        passphrase,
        txs: &txs,
        link: &link,
        config,
        events,
    };

    for bundle in processing {
        let existing = link.list_transactions_for_bundle(&bundle.id).await?;
        if !existing.is_empty() {
            continue;
        }
        if let Err(e) = submit_one(&ctx, &bundle).await {
            warn!(bundle = %bundle.id, error = %e, "executor: bundle submission failed");
            bundles
                .mark_failed(&bundle.id, &format!("submission failed: {e}"), None)
                .await?;
        }
    }
    Ok(())
}

/// The ambient dependencies a single `submit_one` call needs, grouped so the
/// function takes the per-bundle item plus one context rather than 9 args.
struct SubmitCtx<'a> {
    server: &'a Server,
    kp: &'a Keypair,
    pubkey: &'a str,
    passphrase: &'a str,
    txs: &'a TransactionRepo,
    link: &'a BundleTransactionRepo,
    config: &'a Config,
    events: &'a EventBroadcaster,
}

async fn submit_one(
    ctx: &SubmitCtx<'_>,
    bundle: &provider_stack_persistence::OperationsBundle,
) -> anyhow::Result<()> {
    let &SubmitCtx {
        server,
        kp,
        pubkey,
        passphrase,
        txs,
        link,
        config,
        events,
    } = ctx;
    let contract_id = bundle
        .channel_contract_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("bundle is missing channel_contract_id"))?;

    let mut account = server
        .get_account(pubkey)
        .await
        .map_err(|e| anyhow::anyhow!("get_account failed: {e:?}"))?;

    // Decode + aggregate the bundle's MLXDR slots into the single ChannelOperation
    // ScVal that the privacy-channel contract's `transact(op: ChannelOperation)`
    // entrypoint expects. Each slot's operation payload is bucketed by its type byte
    // (Create / Spend / Deposit / Withdraw) into the matching field of the struct.
    let mlxdr_strings = bundle
        .operations_mlxdr
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("operations_mlxdr must be a JSON array"))?;
    let slot_refs: Vec<&str> = mlxdr_strings
        .iter()
        .map(|slot| {
            slot.as_str()
                .ok_or_else(|| anyhow::anyhow!("operations_mlxdr slot must be a string"))
        })
        .collect::<anyhow::Result<_>>()?;

    // Bundle balance: `total_deposit + spend = total_create + total_withdraw`. The
    // fee column on the bundle is the excess (PP's margin). Inject a fresh Create
    // for that amount under a deterministic OPEX UTXO key so the bundle balances.
    let opex_utxo_key = derive_opex_utxo_key(&config.pp_secret_key, &bundle.id);
    let fee_create = mlxdr::build_fee_create_op(&opex_utxo_key, bundle.fee as i128)
        .map_err(|e| anyhow::anyhow!("fee Create op: {e}"))?;
    let channel_op = mlxdr::aggregate_to_channel_operation_with_fee_create(&slot_refs, fee_create)
        .map_err(|e| anyhow::anyhow!("MLXDR aggregate: {e}"))?;

    // Lift the depositors' / withdrawers' pre-signed SorobanAuthorizationEntry
    // structs out of each MLXDR slot. They're built + signed on the client in
    // `MoonlightOperation.signWithEd25519` and committed to a specific nonce +
    // signature_expiration_ledger; we relay them verbatim into the final tx's
    // auth list (no re-derivation — that would invalidate the signature).
    let user_signed = mlxdr::extract_user_signed_entries(&slot_refs)
        .map_err(|e| anyhow::anyhow!("MLXDR user-signed extract: {e}"))?;
    // Each Spend slot also carries a P256 signature over the channel-auth
    // contract's per-spend AuthPayload. These get folded into the same Signatures
    // map as the Provider entry (key = SignerKey::P256(utxo), val = (Signature::P256(sig), exp)).
    let user_spends = mlxdr::extract_user_spend_signatures(&slot_refs)
        .map_err(|e| anyhow::anyhow!("MLXDR spend-sig extract: {e}"))?;

    let contract = Contracts::new(contract_id)
        .map_err(|e| anyhow::anyhow!("Contracts::new failed: {e:?}"))?;
    let op = contract.call("transact", Some(vec![channel_op]));

    let mut builder = TransactionBuilder::new(&mut account, passphrase, None);
    builder.fee(config.network_fee.max(100) as u32);
    builder.add_operation(op);
    let tx = builder.build();

    // Soroban contract calls require simulate+assemble before signing, so the
    // RPC has the resource footprint, auth entries, and minimum fee attached.
    // Without this the network accepts the send but the tx never lands.
    let simulation = server
        .simulate_transaction(&tx, None)
        .await
        .map_err(|e| anyhow::anyhow!("simulate_transaction failed: {e:?}"))?;
    let mut assembled = assemble_transaction(&tx, simulation)
        .map_err(|e| anyhow::anyhow!("assemble_transaction failed: {e:?}"))?;

    // Splice user-pre-signed entries into the auth list: every Account-typed
    // simulation entry is replaced with the matching user's verbatim entry,
    // Contract-typed entries (channel-auth) are kept and will be PP-signed.
    // Mirrors the Deno SDK's `auth: [providerEntry, ...extSignatures.values()]`
    // at `moonlight-sdk/src/transaction-builder/index.ts:645`.
    splice_user_signed_entries(&mut assembled, &user_signed)?;

    // Add the user-signed entries' nonce ledger keys to the footprint — simulation
    // populated the footprint against its placeholder nonces, but the on-chain check
    // reads the actual user-committed nonces. Missing footprint entries trip
    // "trying to access nonce outside of the footprint" before the auth check runs.
    extend_footprint_with_user_nonces(&mut assembled, &user_signed)?;

    // The spliced auth entries reach storage / instructions that simulation didn't
    // exercise; the host enforces the limits set in SorobanTransactionData. Pad the
    // resource budget so on-chain execution has headroom for the user-pre-signed
    // chain.
    inflate_soroban_resources(&mut assembled);

    // Simulation seeds Contract-typed `signature_expiration_ledger` with a
    // placeholder (often 0); by the time the tx executes the on-chain check
    // `valid_until_ledger < current_ledger_seq` trips MoonlightError::SignatureExpired
    // (1010). Set Contract entries to (latest_ledger + offset). User-signed Account
    // entries committed to their own expiration in the signed preimage — leave alone.
    let latest = server
        .get_latest_ledger()
        .await
        .map_err(|e| anyhow::anyhow!("get_latest_ledger for auth exp: {e:?}"))?;
    let expiration = latest
        .sequence
        .saturating_add(config.transaction_expiration_offset);
    set_auth_signature_expiration(&mut assembled, expiration)?;

    // Sign Contract-typed Soroban auth entries with the PP's key. The
    // channel-auth contract's `__check_auth` reads `signatures: Signatures` (a
    // `Map<SignerKey, (Signature, u32)>`) and demands a Provider(Ed25519) entry
    // whose signature covers the auth preimage. assemble_transaction copies the
    // entries from simulation with a placeholder signature; we replace it.
    // Account-typed entries are user-pre-signed and are left untouched.
    let signing_key = signing_key_from_seed(&config.pp_secret_key)
        .map_err(|e| anyhow::anyhow!("PP signing key: {e:?}"))?;
    let pp_pk32 = signing_key.verifying_key().to_bytes();
    sign_soroban_auth_entries(
        &mut assembled,
        passphrase,
        &signing_key,
        &pp_pk32,
        &user_spends,
    )?;

    assembled.sign(std::slice::from_ref(kp));

    let response = server
        .send_transaction(assembled)
        .await
        .map_err(|e| anyhow::anyhow!("send_transaction failed: {e:?}"))?;

    let timeout =
        chrono::Utc::now() + ChronoDuration::seconds(config.transaction_expiration_offset as i64 * 5);
    let latest_ledger_seq = response.latest_ledger.to_string();
    txs.create(&response.hash, timeout, &latest_ledger_seq).await?;
    link.link(&bundle.id, &response.hash).await?;

    events.send(ProviderEvent::executor_transaction_submitted(
        events.current_scope(),
        &response.hash,
        std::slice::from_ref(&bundle.id),
        bundle.channel_contract_id.as_deref(),
    ));

    Ok(())
}

/// Set every Contract-typed auth entry's `signature_expiration_ledger` to the supplied
/// absolute value. Used to overwrite the simulation's placeholder for the channel-auth
/// custom-account entry so the bundle has a real window to land in. Account-typed
/// entries are skipped — those are user-pre-signed and their expiration is part of the
/// committed preimage.
fn set_auth_signature_expiration(
    tx: &mut soroban_client::transaction::Transaction,
    expiration: u32,
) -> anyhow::Result<()> {
    use soroban_client::xdr::{
        InvokeHostFunctionOp, OperationBody, ScAddress, SorobanAuthorizationEntry,
        SorobanCredentials, VecM,
    };
    let Some(ops) = tx.operations.as_mut() else {
        return Ok(());
    };
    for op in ops.iter_mut() {
        let OperationBody::InvokeHostFunction(InvokeHostFunctionOp { host_function, auth }) =
            op.body.clone()
        else {
            continue;
        };
        let mut entries: Vec<SorobanAuthorizationEntry> = auth.into();
        for entry in entries.iter_mut() {
            if let SorobanCredentials::Address(ref mut addr) = entry.credentials {
                if matches!(addr.address, ScAddress::Contract(_)) {
                    addr.signature_expiration_ledger = expiration;
                }
            }
        }
        let new_auth: VecM<SorobanAuthorizationEntry> = VecM::try_from(entries)
            .map_err(|e| anyhow::anyhow!("auth VecM: {e}"))?;
        op.body = OperationBody::InvokeHostFunction(InvokeHostFunctionOp {
            host_function,
            auth: new_auth,
        });
    }
    Ok(())
}

/// Add a `LedgerKey::ContractData(ContractDataDurability::Temporary, key=LedgerKeyNonce)`
/// to the read-write footprint for every user-pre-signed auth entry. Soroban stores a
/// per-(address, nonce) one-time token to prevent replay; the host enforces that this
/// storage slot is in the tx footprint before consuming it. Simulation only listed the
/// placeholder nonces it generated, so the user's actual nonces must be added here.
fn extend_footprint_with_user_nonces(
    tx: &mut soroban_client::transaction::Transaction,
    user_signed: &[mlxdr::UserSignedSlot],
) -> anyhow::Result<()> {
    use soroban_client::xdr::{
        AccountId, ContractDataDurability, LedgerKey, LedgerKeyContractData, PublicKey, ScAddress,
        ScNonceKey, ScVal, SorobanCredentials, Uint256, VecM,
    };
    let Some(sd) = tx.soroban_data.as_mut() else {
        return Ok(());
    };
    let mut rw: Vec<LedgerKey> = sd.resources.footprint.read_write.clone().into();
    for slot in user_signed {
        let SorobanCredentials::Address(addr) = &slot.auth_entry.credentials else {
            continue;
        };
        let nonce = addr.nonce;
        let contract = ScAddress::Account(AccountId(PublicKey::PublicKeyTypeEd25519(Uint256(
            slot.account_pk32,
        ))));
        let key = ScVal::LedgerKeyNonce(ScNonceKey { nonce });
        let ledger_key = LedgerKey::ContractData(LedgerKeyContractData {
            contract,
            key,
            durability: ContractDataDurability::Temporary,
        });
        if !rw.iter().any(|k| k == &ledger_key) {
            rw.push(ledger_key);
        }
    }
    sd.resources.footprint.read_write =
        VecM::try_from(rw).map_err(|e| anyhow::anyhow!("footprint RW VecM: {e}"))?;
    Ok(())
}

/// Inflate the simulated Soroban resource budget so the user-spliced auth chain has
/// CPU + storage headroom. Simulation budgets the assembled tx against placeholder
/// auth entries; the actually-submitted tx walks the user-pre-signed entries which
/// may touch different storage and run more instructions. The on-chain host enforces
/// the limits in `SorobanTransactionData`, so under-budgeted txs trap with
/// "operation instructions exceeds amount specified". Inflate generously — the
/// inclusion fee scales but a few extra stroops are cheaper than a re-submission.
fn inflate_soroban_resources(tx: &mut soroban_client::transaction::Transaction) {
    let Some(sd) = tx.soroban_data.as_mut() else {
        return;
    };
    // 4x CPU + read/write storage budget, 2x the resource fee (fee scales with usage).
    sd.resources.instructions = sd.resources.instructions.saturating_mul(4);
    sd.resources.disk_read_bytes = sd.resources.disk_read_bytes.saturating_mul(4);
    sd.resources.write_bytes = sd.resources.write_bytes.saturating_mul(4);
    sd.resource_fee = sd.resource_fee.saturating_mul(4);
    // Pay for the inflated resource budget at the tx fee layer too.
    tx.fee = tx.fee.saturating_add(sd.resource_fee as u32);
}

/// Replace each Account-typed simulation auth entry with the matching user-pre-signed
/// `SorobanAuthorizationEntry` lifted from the bundle's MLXDR slots. Contract-typed
/// entries (the channel-auth custom-account) are left as-is for PP signing downstream.
///
/// Account-typed simulation entries that have no matching user signature are dropped;
/// they're guaranteed to fail on-chain anyway and would not be authorizable.
fn splice_user_signed_entries(
    tx: &mut soroban_client::transaction::Transaction,
    user_signed: &[mlxdr::UserSignedSlot],
) -> anyhow::Result<()> {
    use soroban_client::xdr::{
        AccountId, InvokeHostFunctionOp, OperationBody, PublicKey, ScAddress,
        SorobanAuthorizationEntry, SorobanCredentials, Uint256, VecM,
    };
    let Some(ops) = tx.operations.as_mut() else {
        return Ok(());
    };
    for op in ops.iter_mut() {
        let OperationBody::InvokeHostFunction(InvokeHostFunctionOp { host_function, auth }) =
            op.body.clone()
        else {
            continue;
        };
        let sim_entries: Vec<SorobanAuthorizationEntry> = auth.into();
        let mut new_entries: Vec<SorobanAuthorizationEntry> = Vec::new();
        for entry in sim_entries.into_iter() {
            let account_pk_opt: Option<[u8; 32]> = match &entry.credentials {
                SorobanCredentials::Address(addr) => match &addr.address {
                    ScAddress::Account(AccountId(PublicKey::PublicKeyTypeEd25519(Uint256(
                        pk,
                    )))) => Some(*pk),
                    _ => None,
                },
                _ => None,
            };
            if let Some(pk) = account_pk_opt {
                if let Some(slot) = user_signed.iter().find(|s| s.account_pk32 == pk) {
                    new_entries.push(slot.auth_entry.clone());
                    continue;
                }
                // No matching user signature — drop the placeholder (it can't be authorized).
                continue;
            }
            new_entries.push(entry);
        }
        let new_auth: VecM<SorobanAuthorizationEntry> = VecM::try_from(new_entries)
            .map_err(|e| anyhow::anyhow!("auth VecM: {e}"))?;
        op.body = OperationBody::InvokeHostFunction(InvokeHostFunctionOp {
            host_function,
            auth: new_auth,
        });
    }
    Ok(())
}

/// Walk the assembled tx's InvokeHostFunctionOp.auth list and, for every entry whose
/// credentials are `Address(...)` (i.e. the contract-account-auth path), compute the
/// `HashIdPreimage::SorobanAuthorization` digest, sign it with the PP's Ed25519 key,
/// and overwrite the placeholder `signature` ScVal with a `Signatures` map shaped as
/// the channel-auth contract expects: `{ Provider(pp_pk_32) → (Ed25519(sig_64), valid_until) }`.
fn sign_soroban_auth_entries(
    tx: &mut soroban_client::transaction::Transaction,
    passphrase: &str,
    signing_key: &ed25519_dalek::SigningKey,
    pp_pk32: &[u8; 32],
    user_spends: &[mlxdr::UserSpendSignature],
) -> anyhow::Result<()> {
    use ed25519_dalek::Signer;
    use sha2::{Digest, Sha256};
    use soroban_client::xdr::{
        HashIdPreimage, HashIdPreimageSorobanAuthorization, InvokeHostFunctionOp, Limits,
        OperationBody, ScAddress, ScBytes, ScMap, ScMapEntry, ScSymbol, ScVal, ScVec,
        SorobanAuthorizationEntry, SorobanCredentials, StringM, VecM, WriteXdr,
    };

    let network_id = {
        let h = Sha256::digest(passphrase.as_bytes());
        soroban_client::xdr::Hash(h.into())
    };

    let Some(ops) = tx.operations.as_mut() else {
        return Ok(());
    };
    for op in ops.iter_mut() {
        let OperationBody::InvokeHostFunction(InvokeHostFunctionOp { host_function, auth }) =
            op.body.clone()
        else {
            continue;
        };
        let auth_vec: Vec<SorobanAuthorizationEntry> = auth.into();
        let mut signed_entries: Vec<SorobanAuthorizationEntry> = Vec::new();
        for entry in auth_vec.into_iter() {
            let mut entry = entry.clone();
            if let SorobanCredentials::Address(ref mut addr) = entry.credentials {
                // Skip Account-typed entries — user-pre-signed, relayed verbatim.
                if !matches!(addr.address, ScAddress::Contract(_)) {
                    signed_entries.push(entry);
                    continue;
                }
                let preimage = HashIdPreimage::SorobanAuthorization(
                    HashIdPreimageSorobanAuthorization {
                        network_id: network_id.clone(),
                        nonce: addr.nonce,
                        signature_expiration_ledger: addr.signature_expiration_ledger,
                        invocation: entry.root_invocation.clone(),
                    },
                );
                let preimage_xdr = preimage
                    .to_xdr(Limits::none())
                    .map_err(|e| anyhow::anyhow!("preimage XDR: {e}"))?;
                let digest = Sha256::digest(&preimage_xdr);
                let sig = signing_key.sign(&digest);

                // SignerKey::Provider(BytesN<32>) → ScVal::Vec([Symbol("Provider"), Bytes(pp_pk32)])
                let pp_bytes: soroban_client::xdr::BytesM = pp_pk32
                    .to_vec()
                    .try_into()
                    .map_err(|e: soroban_client::xdr::Error| anyhow::anyhow!("pp bytes: {e}"))?;
                let signer_provider = ScVal::Vec(Some(ScVec(
                    VecM::try_from(vec![
                        ScVal::Symbol(ScSymbol(
                            StringM::<32>::try_from(b"Provider".to_vec())
                                .map_err(|e| anyhow::anyhow!("sym: {e}"))?,
                        )),
                        ScVal::Bytes(ScBytes(pp_bytes)),
                    ])
                    .map_err(|e| anyhow::anyhow!("signer vec: {e}"))?,
                )));
                // Signature::Ed25519(BytesN<64>) → ScVal::Vec([Symbol("Ed25519"), Bytes(sig_64)])
                let sig_bytes: soroban_client::xdr::BytesM = sig
                    .to_bytes()
                    .to_vec()
                    .try_into()
                    .map_err(|e: soroban_client::xdr::Error| anyhow::anyhow!("sig bytes: {e}"))?;
                let sig_ed25519 = ScVal::Vec(Some(ScVec(
                    VecM::try_from(vec![
                        ScVal::Symbol(ScSymbol(
                            StringM::<32>::try_from(b"Ed25519".to_vec())
                                .map_err(|e| anyhow::anyhow!("sym: {e}"))?,
                        )),
                        ScVal::Bytes(ScBytes(sig_bytes)),
                    ])
                    .map_err(|e| anyhow::anyhow!("sig vec: {e}"))?,
                )));
                // (Signature, u32) → ScVal::Vec([sig, U32])
                let provider_sig_tuple = ScVal::Vec(Some(ScVec(
                    VecM::try_from(vec![sig_ed25519, ScVal::U32(addr.signature_expiration_ledger)])
                        .map_err(|e| anyhow::anyhow!("sig tuple: {e}"))?,
                )));

                // For Spend operations, fold the user-pre-signed P256 entries into
                // the same map. Sort by UTXO key bytes so the Soroban host's
                // map-ordering invariant holds (mirrors moonlight-sdk's
                // orderedSpendSigners at signatures-xdr.ts:22).
                let mut sorted_spends: Vec<&mlxdr::UserSpendSignature> = user_spends.iter().collect();
                sorted_spends.sort_by(|a, b| a.utxo_pk65.cmp(&b.utxo_pk65));

                let mut map_entries: Vec<ScMapEntry> = Vec::new();
                for spend in &sorted_spends {
                    let utxo_bytes: soroban_client::xdr::BytesM = spend
                        .utxo_pk65
                        .to_vec()
                        .try_into()
                        .map_err(|e: soroban_client::xdr::Error| {
                            anyhow::anyhow!("p256 utxo bytes: {e}")
                        })?;
                    let signer_p256 = ScVal::Vec(Some(ScVec(
                        VecM::try_from(vec![
                            ScVal::Symbol(ScSymbol(
                                StringM::<32>::try_from(b"P256".to_vec())
                                    .map_err(|e| anyhow::anyhow!("sym: {e}"))?,
                            )),
                            ScVal::Bytes(ScBytes(utxo_bytes)),
                        ])
                        .map_err(|e| anyhow::anyhow!("p256 signer vec: {e}"))?,
                    )));
                    let p256_sig_bytes: soroban_client::xdr::BytesM = spend
                        .sig
                        .to_vec()
                        .try_into()
                        .map_err(|e: soroban_client::xdr::Error| {
                            anyhow::anyhow!("p256 sig bytes: {e}")
                        })?;
                    let p256_sig_variant = ScVal::Vec(Some(ScVec(
                        VecM::try_from(vec![
                            ScVal::Symbol(ScSymbol(
                                StringM::<32>::try_from(b"P256".to_vec())
                                    .map_err(|e| anyhow::anyhow!("sym: {e}"))?,
                            )),
                            ScVal::Bytes(ScBytes(p256_sig_bytes)),
                        ])
                        .map_err(|e| anyhow::anyhow!("p256 sig vec: {e}"))?,
                    )));
                    let p256_tuple = ScVal::Vec(Some(ScVec(
                        VecM::try_from(vec![p256_sig_variant, ScVal::U32(spend.exp)])
                            .map_err(|e| anyhow::anyhow!("p256 tuple: {e}"))?,
                    )));
                    map_entries.push(ScMapEntry {
                        key: signer_p256,
                        val: p256_tuple,
                    });
                }
                // Provider entry last — keys ordered so "P256" symbol entries precede
                // "Provider" entries (alphabetic on the variant symbol name).
                map_entries.push(ScMapEntry {
                    key: signer_provider,
                    val: provider_sig_tuple,
                });

                // Signatures(Map<SignerKey, (Signature, u32)>) — tuple struct under
                // `#[contracttype]` → ScVal::Vec of positional field values; one
                // field, so a single-element vec wrapping the inner Map. Matches
                // moonlight-sdk's buildSignaturesXDR wire format.
                let inner_map = ScVal::Map(Some(ScMap(
                    VecM::try_from(map_entries)
                        .map_err(|e| anyhow::anyhow!("sig map: {e}"))?,
                )));
                let signatures_scval = ScVal::Vec(Some(ScVec(
                    VecM::try_from(vec![inner_map])
                        .map_err(|e| anyhow::anyhow!("sig wrap: {e}"))?,
                )));
                addr.signature = signatures_scval;
            }
            signed_entries.push(entry);
        }
        let new_auth: VecM<SorobanAuthorizationEntry> = VecM::try_from(signed_entries)
            .map_err(|e| anyhow::anyhow!("auth VecM: {e}"))?;
        op.body = OperationBody::InvokeHostFunction(InvokeHostFunctionOp {
            host_function: host_function.clone(),
            auth: new_auth,
        });
    }
    Ok(())
}

/// Derive a per-bundle 65-byte OPEX UTXO key (P-256 uncompressed point shape: `0x04 || X || Y`).
/// Deterministic from (PP secret, bundle id) so retries map to the same key. The contract treats
/// Create UTXO keys as opaque storage keys — no point validation at create time.
fn derive_opex_utxo_key(pp_secret: &str, bundle_id: &str) -> [u8; 65] {
    use sha2::{Digest, Sha256};
    let mut out = [0u8; 65];
    out[0] = 0x04;
    let mut h1 = Sha256::new();
    h1.update(pp_secret.as_bytes());
    h1.update(b"|opex-x|");
    h1.update(bundle_id.as_bytes());
    out[1..33].copy_from_slice(&h1.finalize());
    let mut h2 = Sha256::new();
    h2.update(pp_secret.as_bytes());
    h2.update(b"|opex-y|");
    h2.update(bundle_id.as_bytes());
    out[33..65].copy_from_slice(&h2.finalize());
    out
}

fn network_passphrase_for(network: &str) -> &'static str {
    match network {
        "mainnet" => Networks::public(),
        "testnet" => Networks::testnet(),
        _ => Networks::standalone(),
    }
}
