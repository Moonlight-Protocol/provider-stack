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

use crate::config::Config;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use chrono::Duration as ChronoDuration;
use provider_stack_persistence::{
    BundleStatus, BundleTransactionRepo, OperationsBundleRepo, PgPool, TransactionRepo,
};
use soroban_client::contract::{ContractBehavior, Contracts};
use soroban_client::keypair::{Keypair, KeypairBehavior};
use soroban_client::network::{NetworkPassphrase, Networks};
use soroban_client::transaction::{TransactionBehavior, TransactionBuilder, TransactionBuilderBehavior};
use soroban_client::xdr::{Limits, ScVal, WriteXdr};
use soroban_client::{Options, Server};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{interval, MissedTickBehavior};
use tracing::{debug, instrument, warn};

#[instrument(skip_all, name = "pipeline.executor")]
pub async fn run(config: Arc<Config>, pool: PgPool) {
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

        if let Err(e) = run_tick(&server, &pool, &config).await {
            warn!(error = %e, "executor tick failed");
        }
        debug!("executor tick complete");

        let mut guard = processing.lock().await;
        *guard = false;
    }
}

/// One executor tick. Exposed for the integration test.
pub async fn run_tick(server: &Server, pool: &PgPool, config: &Config) -> anyhow::Result<()> {
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

    for bundle in processing {
        if let Err(e) = submit_one(server, &kp, &pubkey, passphrase, &bundle, &txs, &link, config).await {
            warn!(bundle = %bundle.id, error = %e, "executor: bundle submission failed");
            bundles
                .mark_failed(&bundle.id, &format!("submission failed: {e}"), None)
                .await?;
        }
    }
    Ok(())
}

async fn submit_one(
    server: &Server,
    kp: &Keypair,
    pubkey: &str,
    passphrase: &str,
    bundle: &provider_stack_persistence::OperationsBundle,
    txs: &TransactionRepo,
    link: &BundleTransactionRepo,
    config: &Config,
) -> anyhow::Result<()> {
    let contract_id = bundle
        .channel_contract_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("bundle is missing channel_contract_id"))?;

    let mut account = server
        .get_account(pubkey)
        .await
        .map_err(|e| anyhow::anyhow!("get_account failed: {e:?}"))?;

    // Decode operations_mlxdr (jsonb array of base64 strings) → Vec<ScVal::Bytes>.
    let mlxdr_strings = bundle
        .operations_mlxdr
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("operations_mlxdr must be a JSON array"))?;
    let mut args: Vec<ScVal> = Vec::with_capacity(mlxdr_strings.len());
    for slot in mlxdr_strings {
        let s = slot
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("operations_mlxdr slot must be a string"))?;
        let decoded = B64
            .decode(s)
            .map_err(|e| anyhow::anyhow!("operations_mlxdr base64 decode: {e}"))?;
        let bytes_m: soroban_client::xdr::BytesM = decoded
            .try_into()
            .map_err(|e: soroban_client::xdr::Error| anyhow::anyhow!("BytesM: {e}"))?;
        args.push(ScVal::Bytes(soroban_client::xdr::ScBytes(bytes_m)));
    }

    let contract = Contracts::new(contract_id)
        .map_err(|e| anyhow::anyhow!("Contracts::new failed: {e:?}"))?;
    let op = contract.call("transact", Some(args));

    let mut builder = TransactionBuilder::new(&mut account, passphrase, None);
    builder.fee(config.network_fee.max(100) as u32);
    builder.add_operation(op);
    let mut tx = builder.build();
    tx.sign(&[kp.clone()]);

    let response = server
        .send_transaction(tx)
        .await
        .map_err(|e| anyhow::anyhow!("send_transaction failed: {e:?}"))?;

    let timeout =
        chrono::Utc::now() + ChronoDuration::seconds(config.transaction_expiration_offset as i64 * 5);
    let latest_ledger_seq = response.latest_ledger.to_string();
    txs.create(&response.hash, timeout, &latest_ledger_seq).await?;
    link.link(&bundle.id, &response.hash).await?;
    Ok(())
}

fn network_passphrase_for(network: &str) -> &'static str {
    match network {
        "mainnet" => Networks::public(),
        "testnet" => Networks::testnet(),
        _ => Networks::standalone(),
    }
}
