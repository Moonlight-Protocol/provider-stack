use crate::config::Config;
use crate::events::EventBroadcaster;
use provider_stack_persistence::PgPool;
use std::sync::Arc;
use tokio::time::{interval, MissedTickBehavior};
use tracing::{debug, instrument};

/// Verifier loop: polls Soroban RPC `get_transaction` for unverified tx hashes, updates
/// status, broadcasts per-operation events to WebSocket subscribers.
///
/// **Status**: scaffold — polling + broadcast port next.
#[instrument(skip_all, name = "pipeline.verifier")]
pub async fn run(config: Arc<Config>, _pool: PgPool, _events: EventBroadcaster) {
    let mut tick = interval(config.mempool.verifier_interval);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        debug!("verifier tick");
        // TODO: poll get_transaction(hash) for UNVERIFIED, mark COMPLETED/FAILED, broadcast events
    }
}
