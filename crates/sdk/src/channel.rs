//! Channel-contract reads.
//!
//! `utxo_balances(utxos: Vec<BytesN<65>>) -> Vec<i128>` is the only read used during the
//! bundle-submit path; we hit it via `Server::simulate_transaction`. The wire helper here
//! abstracts the simulate-and-decode into one method so the API crate doesn't have to
//! deal with operation builders or ScVal traversal.

use anyhow::{anyhow, Context, Result};
use soroban_client::account::{Account, AccountBehavior};
use soroban_client::contract::{ContractBehavior, Contracts};
use soroban_client::network::Networks;
use soroban_client::transaction::{TransactionBuilder, TransactionBuilderBehavior};
use soroban_client::xdr::{Int128Parts, Limits, ReadXdr, ScBytes, ScVal};
use soroban_client::Server;

/// Read on-chain UTXO balances for the supplied 65-byte UTXO pubkeys.
///
/// Returns balances in the same order as the input. Missing UTXOs are reported as `-1`
/// per the on-chain contract's convention (see `moonlight-utxo-core`).
pub async fn fetch_utxo_balances(
    server: &Server,
    channel_contract_id: &str,
    network: &str,
    pp_pubkey: &str,
    utxos: Vec<Vec<u8>>,
) -> Result<Vec<i128>> {
    if utxos.is_empty() {
        return Ok(Vec::new());
    }

    let contract =
        Contracts::new(channel_contract_id).map_err(|e| anyhow!("Contracts::new: {e:?}"))?;

    // Simulate-only: we need any source account that exists on chain (or a dummy with
    // sequence 0; simulate doesn't validate the account). Use a fresh PP-shaped Account.
    let mut account = Account::new(pp_pubkey, "0").map_err(|e| anyhow!("Account::new: {e:?}"))?;

    let utxos_scval = ScVal::Vec(Some(soroban_client::xdr::ScVec(
        soroban_client::xdr::VecM::try_from(
            utxos
                .into_iter()
                .map(|u| -> Result<ScVal> {
                    Ok(ScVal::Bytes(ScBytes(u.try_into().map_err(
                        |e: soroban_client::xdr::Error| anyhow!("BytesM: {e}"),
                    )?)))
                })
                .collect::<Result<Vec<_>>>()?,
        )
        .map_err(|e| anyhow!("VecM: {e}"))?,
    )));

    let op = contract.call("utxo_balances", Some(vec![utxos_scval]));

    let mut builder = TransactionBuilder::new(&mut account, network_passphrase(network), None);
    builder.fee(100u32).add_operation(op);
    let tx = builder.build_for_simulation();

    let sim = server
        .simulate_transaction(&tx, None)
        .await
        .map_err(|e| anyhow!("simulate_transaction: {e:?}"))?;

    // sim.to_result() returns Option<(ScVal, Vec<auth>)> per soroban-client.
    let (return_val, _auth) = sim
        .to_result()
        .context("simulation returned no result for utxo_balances")?;

    parse_balances(&return_val)
}

fn parse_balances(v: &ScVal) -> Result<Vec<i128>> {
    let scvec = match v {
        ScVal::Vec(Some(soroban_client::xdr::ScVec(v))) => v,
        _ => return Err(anyhow!("expected ScVec result from utxo_balances")),
    };
    let mut out = Vec::with_capacity(scvec.len());
    for item in scvec.iter() {
        match item {
            ScVal::I128(Int128Parts { hi, lo }) => {
                out.push(((*hi as i128) << 64) | (*lo as i128));
            }
            _ => return Err(anyhow!("expected ScI128 in utxo_balances result")),
        }
    }
    Ok(out)
}

fn network_passphrase(network: &str) -> &'static str {
    use soroban_client::network::NetworkPassphrase;
    match network {
        "mainnet" => Networks::public(),
        "testnet" => Networks::testnet(),
        _ => Networks::standalone(),
    }
}

/// Helper exposed for tests / callsites that already have an in-memory ScVal Vec response —
/// e.g. when mocking simulate_transaction at the JSON-RPC layer.
pub fn decode_balances_scval(v: &str) -> Result<Vec<i128>> {
    let parsed = ScVal::from_xdr_base64(v, Limits::none())
        .map_err(|e| anyhow!("ScVal::from_xdr_base64: {e}"))?;
    parse_balances(&parsed)
}
