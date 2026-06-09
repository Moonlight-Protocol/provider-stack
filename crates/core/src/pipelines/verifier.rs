//! Verifier pipeline.
//!
//! For every UNVERIFIED `transactions` row, poll Soroban RPC `getTransaction` by hash:
//!  - `SUCCESS`  → mark VERIFIED, broadcast `BundleCompleted` for each linked bundle, mark
//!                 each bundle COMPLETED.
//!  - `FAILED`   → mark FAILED, broadcast `BundleFailed`, mark each linked bundle FAILED.
//!  - `NOT_FOUND` → leave UNVERIFIED, retry next tick.
//!
//! The loop is gated by a `Mutex<bool>` so overlapping ticks collapse safely.

use crate::config::Config;
use crate::events::{EventBroadcaster, ProviderEvent};
use provider_stack_persistence::{
    BundleStatus, BundleTransactionRepo, OperationsBundleRepo, PgPool, TransactionRepo,
    TransactionStatus,
};
use soroban_client::soroban_rpc::TransactionStatus as RpcStatus;
use soroban_client::{Options, Server};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{interval, MissedTickBehavior};
use tracing::{debug, instrument, warn};

#[instrument(skip_all, name = "pipeline.verifier")]
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

        if let Err(e) = run_tick(&server, &pool, &events).await {
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
) -> anyhow::Result<()> {
    let tx_repo = TransactionRepo::new(pool.clone());
    let bundle_link = BundleTransactionRepo::new(pool.clone());
    let bundle_repo = OperationsBundleRepo::new(pool.clone());

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
                for bid in &bundle_ids {
                    bundle_repo.set_status(bid, BundleStatus::Completed).await?;
                    events.send(ProviderEvent::BundleCompleted {
                        bundle_id: bid.clone(),
                        tx_hash: tx.id.clone(),
                    });
                }
            }
            RpcStatus::Failed => {
                tx_repo.set_status(&tx.id, TransactionStatus::Failed).await?;
                for bid in &bundle_ids {
                    bundle_repo
                        .mark_failed(bid, "tx failed on chain", None)
                        .await?;
                    events.send(ProviderEvent::BundleFailed {
                        bundle_id: bid.clone(),
                        reason: "tx failed on chain".into(),
                    });
                }
            }
            RpcStatus::NotFound => {
                // Tx not yet observed on chain — retry next tick.
            }
        }
    }
    Ok(())
}
