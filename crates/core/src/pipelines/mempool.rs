use crate::config::Config;
use provider_stack_persistence::{BundleStatus, OperationsBundleRepo, PgPool};
use std::sync::Arc;
use tokio::time::{interval, MissedTickBehavior};
use tracing::{debug, instrument, warn};

/// Mempool processor: sweeps expired bundles + promotes PENDING → PROCESSING when slots free.
///
/// **Status**: scaffold — expiry sweep is wired; weight-based slot management ports next.
#[instrument(skip_all, name = "pipeline.mempool")]
pub async fn run(config: Arc<Config>, pool: PgPool) {
    let mut tick = interval(config.mempool.ttl_check_interval);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let repo = OperationsBundleRepo::new(pool);

    loop {
        tick.tick().await;
        if let Err(e) = sweep_expired(&repo).await {
            warn!(error = %e, "mempool sweep failed");
        }
        debug!("mempool tick");
    }
}

async fn sweep_expired(repo: &OperationsBundleRepo) -> anyhow::Result<()> {
    // Bundles past TTL → EXPIRED. Real impl will batch + log; scaffold is a no-op iterator.
    let pending: Vec<provider_stack_persistence::OperationsBundle> =
        repo.list_by_status(BundleStatus::Pending, 64).await?;
    let now = chrono::Utc::now();
    for b in pending.into_iter().filter(|b| b.ttl < now) {
        repo.set_status(&b.id, BundleStatus::Expired).await?;
    }
    Ok(())
}
