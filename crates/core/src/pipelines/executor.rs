use crate::config::Config;
use provider_stack_persistence::PgPool;
use std::sync::Arc;
use tokio::time::{interval, MissedTickBehavior};
use tracing::{debug, instrument};

/// Executor loop: pulls PROCESSING bundles, builds + signs the Soroban tx with the PP key,
/// submits via `soroban-client`, records the tx hash.
///
/// **Status**: scaffold — PP key loading + soroban-client invocation port next.
#[instrument(skip_all, name = "pipeline.executor")]
pub async fn run(config: Arc<Config>, _pool: PgPool) {
    let mut tick = interval(config.mempool.executor_interval);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        debug!("executor tick");
        // TODO: pull bundles in PROCESSING, build channel tx, sign with PP key, send_transaction
    }
}
