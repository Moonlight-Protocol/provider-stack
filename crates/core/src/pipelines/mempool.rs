//! Mempool processor.
//!
//! Two responsibilities:
//! 1. **Sweep expired**: any `PENDING` bundle whose `ttl` has passed is marked `EXPIRED`.
//! 2. **Promote**: while live `PROCESSING` slot count < `MEMPOOL_SLOT_CAPACITY`, promote the
//!    oldest `PENDING` bundle to `PROCESSING`. The executor pipeline picks it up from there.
//!
//! Both run on every tick of `MEMPOOL_TTL_CHECK_INTERVAL_MS`. A `Mutex<bool>` is held for the
//! duration of one tick so overlapping intervals collapse harmlessly.

use crate::config::Config;
use provider_stack_persistence::{BundleStatus, OperationsBundleRepo, PgPool};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{interval, MissedTickBehavior};
use tracing::{debug, instrument, warn};

#[instrument(skip_all, name = "pipeline.mempool")]
pub async fn run(config: Arc<Config>, pool: PgPool) {
    let mut tick = interval(config.mempool.ttl_check_interval);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let repo = OperationsBundleRepo::new(pool);
    let processing = Arc::new(Mutex::new(false));

    loop {
        tick.tick().await;
        let mut guard = processing.lock().await;
        if *guard {
            continue;
        }
        *guard = true;
        drop(guard);

        if let Err(e) = run_tick(&repo, &config).await {
            warn!(error = %e, "mempool tick failed");
        }
        debug!("mempool tick complete");

        let mut guard = processing.lock().await;
        *guard = false;
    }
}

/// One mempool tick: sweep expired, then promote up to slot capacity.
pub async fn run_tick(repo: &OperationsBundleRepo, config: &Config) -> anyhow::Result<()> {
    let now = chrono::Utc::now();

    // 1. Sweep expired (PENDING + ttl < now → EXPIRED).
    let pending: Vec<provider_stack_persistence::OperationsBundle> =
        repo.list_by_status(BundleStatus::Pending, 256).await?;
    let (expired, fresh): (Vec<_>, Vec<_>) =
        pending.into_iter().partition(|b| b.ttl < now);
    for b in &expired {
        repo.set_status(&b.id, BundleStatus::Expired).await?;
    }

    // 2. Promote up to slot_capacity, oldest-first (already sorted desc by created_at — reverse).
    let processing_now: Vec<provider_stack_persistence::OperationsBundle> = repo
        .list_by_status(BundleStatus::Processing, config.mempool.slot_capacity as i64)
        .await?;
    let free_slots = config
        .mempool
        .slot_capacity
        .saturating_sub(processing_now.len());

    let mut fresh = fresh;
    fresh.sort_by_key(|b| b.created_at);
    for b in fresh.into_iter().take(free_slots) {
        repo.set_status(&b.id, BundleStatus::Processing).await?;
    }

    Ok(())
}
