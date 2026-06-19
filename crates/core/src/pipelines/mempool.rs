//! Mempool processor.
//!
//! Two responsibilities:
//! 1. **Sweep expired**: any `PENDING` bundle whose `ttl` has passed is marked `EXPIRED`.
//! 2. **Promote**: while live `PROCESSING` slot count < `MEMPOOL_SLOT_CAPACITY`, promote the
//!    oldest `PENDING` bundle to `PROCESSING`. The executor pipeline picks it up from there.
//!    On every promotion, emit `mempool.bundle_added` with the weight + amount derived
//!    from the bundle's MLXDR + the submitter's entity name + jurisdictions.
//!
//! Both run on every tick of `MEMPOOL_TTL_CHECK_INTERVAL_MS`. A `Mutex<bool>` is held for the
//! duration of one tick so overlapping intervals collapse harmlessly.

use crate::config::Config;
use crate::events::{summarize_bundle, EventBroadcaster, ProviderEvent, Submitter};
use provider_stack_persistence::{
    BundleStatus, EntityRepo, OperationsBundleRepo, PgPool,
};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{interval, MissedTickBehavior};
use tracing::{debug, instrument, warn, Instrument};

#[instrument(skip_all, name = "pipeline.mempool")]
pub async fn run(config: Arc<Config>, pool: PgPool, events: EventBroadcaster) {
    let mut tick = interval(config.mempool.ttl_check_interval);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let repo = OperationsBundleRepo::new(pool.clone());
    let entities = EntityRepo::new(pool);
    let processing = Arc::new(Mutex::new(false));

    loop {
        tick.tick().await;
        let mut guard = processing.lock().await;
        if *guard {
            continue;
        }
        *guard = true;
        drop(guard);

        let tick_span = tracing::info_span!("Mempool.tick");
        if let Err(e) = run_tick(&repo, &entities, &config, &events).instrument(tick_span).await {
            warn!(error = %e, "mempool tick failed");
        }
        debug!("mempool tick complete");

        let mut guard = processing.lock().await;
        *guard = false;
    }
}

/// One mempool tick: sweep expired, then promote up to slot capacity.
pub async fn run_tick(
    repo: &OperationsBundleRepo,
    entities: &EntityRepo,
    config: &Config,
    events: &EventBroadcaster,
) -> anyhow::Result<()> {
    let now = chrono::Utc::now();

    // 1. Sweep expired (PENDING + ttl < now → EXPIRED).
    let pending: Vec<provider_stack_persistence::OperationsBundle> =
        repo.list_by_status(BundleStatus::Pending, 256).await?;
    let (expired, fresh): (Vec<_>, Vec<_>) =
        pending.into_iter().partition(|b| b.ttl < now);
    for b in &expired {
        repo.set_status(&b.id, BundleStatus::Expired).await?;
    }

    // 2. Promote up to slot_capacity, oldest-first.
    let processing_now: Vec<provider_stack_persistence::OperationsBundle> = repo
        .list_by_status(BundleStatus::Processing, config.mempool.slot_capacity as i64)
        .await?;
    let processing_count_before = processing_now.len();
    let free_slots = config
        .mempool
        .slot_capacity
        .saturating_sub(processing_count_before);

    let mut fresh = fresh;
    fresh.sort_by_key(|b| b.created_at);
    let mut promotions = 0usize;
    for b in fresh.into_iter().take(free_slots) {
        repo.set_status(&b.id, BundleStatus::Processing).await?;

        let summary = match summarize_bundle(
            &b.operations_mlxdr,
            config.mempool.cheap_op_weight,
            config.mempool.expensive_op_weight,
        ) {
            Ok(s) => s,
            Err(e) => {
                warn!(bundle = %b.id, error = %e, "mempool: summarize failed");
                continue;
            }
        };
        let (entity_name, jurisdictions): (Option<String>, Vec<String>) = match b
            .created_by
            .as_deref()
        {
            Some(eid) => match entities.find_by_id(eid).await {
                Ok(Some(entity)) => (
                    entity.name,
                    entity.jurisdictions.unwrap_or_default(),
                ),
                _ => (None, Vec::new()),
            },
            None => (None, Vec::new()),
        };
        let new_slot = (processing_count_before + promotions) == 0;
        let ev = ProviderEvent::mempool_bundle_added(
            events.current_scope(),
            &b.id,
            summary.weight,
            b.channel_contract_id.as_deref(),
            new_slot,
            Submitter {
                name: entity_name,
                jurisdictions,
            },
            summary.primary_amount,
        );
        events.send(ev);
        promotions += 1;
    }

    Ok(())
}
