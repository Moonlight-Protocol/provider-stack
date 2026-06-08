//! Long-lived pipelines spawned at boot. Each is a `tokio::spawn` task with periodic ticks
//! gated by an `isProcessing` flag, mirroring the Deno reference.

pub mod event_watcher;
pub mod executor;
pub mod mempool;
pub mod metrics;
pub mod verifier;

use crate::config::Config;
use crate::events::EventBroadcaster;
use provider_stack_persistence::PgPool;
use std::sync::Arc;
use tokio::task::JoinHandle;

/// Start every long-lived pipeline. Returns the join handles so `main` can `select!` on them
/// or shut them down cleanly on signal.
pub fn spawn_all(
    config: Arc<Config>,
    pool: PgPool,
    events: EventBroadcaster,
) -> Vec<JoinHandle<()>> {
    vec![
        tokio::spawn(mempool::run(config.clone(), pool.clone())),
        tokio::spawn(executor::run(config.clone(), pool.clone())),
        tokio::spawn(verifier::run(config.clone(), pool.clone(), events.clone())),
        tokio::spawn(event_watcher::run(config.clone(), pool.clone())),
        tokio::spawn(metrics::run(config, pool)),
    ]
}
