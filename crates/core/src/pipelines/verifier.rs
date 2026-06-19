//! Verifier pipeline.
//!
//! For every UNVERIFIED `transactions` row, poll Soroban RPC `getTransaction` by hash:
//!  - `SUCCESS` → mark VERIFIED, broadcast `BundleCompleted` per linked bundle, mark each COMPLETED.
//!  - `FAILED` → mark FAILED, broadcast `BundleFailed`, mark each linked bundle FAILED.
//!  - `NOT_FOUND` → leave UNVERIFIED, retry next tick.
//!
//! The loop is gated by a `Mutex<bool>` so overlapping ticks collapse safely.

use crate::config::Config;
use crate::events::{summarize_bundle, EventBroadcaster, ProviderEvent};
use provider_stack_persistence::{
    BundleStatus, BundleTransactionRepo, OperationsBundleRepo, PgPool, TransactionRepo,
    TransactionStatus,
};
use soroban_client::soroban_rpc::TransactionStatus as RpcStatus;
use soroban_client::{Options, Server};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{interval, MissedTickBehavior};
use tracing::{debug, instrument, warn, Instrument};

#[instrument(skip_all, name = "pipeline.verifier")]
pub async fn run(config: Arc<Config>, pool: PgPool, events: EventBroadcaster) {
    let cheap = config.mempool.cheap_op_weight;
    let expensive = config.mempool.expensive_op_weight;
    let _ = (cheap, expensive);
    let server = match Server::new(
        &config.stellar_rpc_url,
        Options {
            allow_http: true,
            ..Options::default()
        },
    ) {
        Ok(s) => s,
        Err(e) => {
            warn!(error = ?e, "verifier: Server::new failed; pipeline will not run");
            return;
        }
    };

    let mut tick = interval(config.mempool.verifier_interval);
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

        let tick_span = tracing::info_span!("Verifier.tick");
        if let Err(e) = run_tick(&server, &pool, &events, &config).instrument(tick_span).await {
            warn!(error = %e, "verifier tick failed");
        }
        debug!("verifier tick complete");

        let mut guard = processing.lock().await;
        *guard = false;
    }
}

/// One verifier tick. Exposed for the integration test.
pub async fn run_tick(
    server: &Server,
    pool: &PgPool,
    events: &EventBroadcaster,
    config: &Config,
) -> anyhow::Result<()> {
    let tx_repo = TransactionRepo::new(pool.clone());
    let bundle_link = BundleTransactionRepo::new(pool.clone());
    let bundle_repo = OperationsBundleRepo::new(pool.clone());

    let cheap = config.mempool.cheap_op_weight;
    let expensive = config.mempool.expensive_op_weight;

    let unverified = tx_repo.list_unverified(64).await?;
    for tx in unverified {
        let result = server.get_transaction(&tx.id).await;
        let resp = match result {
            Ok(r) => r,
            Err(e) => {
                warn!(tx = %tx.id, error = ?e, "get_transaction failed");
                continue;
            }
        };
        let bundle_ids = bundle_link.list_bundles_for_transaction(&tx.id).await?;
        match resp.status {
            RpcStatus::Success => {
                tx_repo.set_status(&tx.id, TransactionStatus::Verified).await?;

                // One verifier.bundle_completed event for the whole tx batch,
                // then a kind-specific bundle.{deposit,withdraw}_completed event
                // per bundle if its primary flow has an external counterparty.
                let scope = events.current_scope();
                let channel_id = match bundle_ids.first() {
                    Some(bid) => match bundle_repo.find_by_id(bid).await? {
                        Some(b) => b.channel_contract_id.clone(),
                        None => None,
                    },
                    None => None,
                };
                events.send(ProviderEvent::verifier_bundle_completed(
                    scope.clone(),
                    &tx.id,
                    &bundle_ids,
                    channel_id.as_deref(),
                ));

                for bid in &bundle_ids {
                    bundle_repo.set_status(bid, BundleStatus::Completed).await?;
                    let Some(bundle) = bundle_repo.find_by_id(bid).await? else {
                        continue;
                    };
                    let summary = match summarize_bundle(
                        &bundle.operations_mlxdr,
                        cheap,
                        expensive,
                    ) {
                        Ok(s) => s,
                        Err(e) => {
                            warn!(bundle = %bid, error = %e, "verifier: summarize failed");
                            continue;
                        }
                    };
                    let amount = summary.primary_amount.clone().unwrap_or_default();
                    match summary.primary_kind {
                        "deposit" => {
                            if let Some(addr) = summary.depositor_address {
                                events.send(ProviderEvent::bundle_deposit_completed(
                                    scope.clone(),
                                    bid,
                                    &tx.id,
                                    bundle.channel_contract_id.as_deref(),
                                    &addr,
                                    &amount,
                                ));
                            }
                        }
                        "withdraw" => {
                            if let Some(addr) = summary.recipient_address {
                                events.send(ProviderEvent::bundle_withdraw_completed(
                                    scope.clone(),
                                    bid,
                                    &tx.id,
                                    bundle.channel_contract_id.as_deref(),
                                    &addr,
                                    &amount,
                                ));
                            }
                        }
                        _ => {}
                    }
                }
            }
            RpcStatus::Failed => {
                tx_repo.set_status(&tx.id, TransactionStatus::Failed).await?;
                for bid in &bundle_ids {
                    bundle_repo
                        .mark_failed(bid, "tx failed on chain", None)
                        .await?;
                }
                let channel_id = match bundle_ids.first() {
                    Some(bid) => bundle_repo
                        .find_by_id(bid)
                        .await?
                        .and_then(|b| b.channel_contract_id),
                    None => None,
                };
                events.send(ProviderEvent::verifier_bundle_failed(
                    events.current_scope(),
                    &tx.id,
                    &bundle_ids,
                    channel_id.as_deref(),
                ));
            }
            RpcStatus::NotFound => {
                // Tx not yet observed on chain — retry next tick.
            }
        }
    }
    Ok(())
}
