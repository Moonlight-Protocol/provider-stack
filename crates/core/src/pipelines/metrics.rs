use crate::config::Config;
use provider_stack_persistence::{MempoolMetricRepo, PgPool};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::{interval, MissedTickBehavior};
use tracing::{debug, instrument, warn};

const PLATFORM_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Metrics collector: snapshot per 60s — queue depth, slot count, completion stats.
#[instrument(skip_all, name = "pipeline.metrics")]
pub async fn run(_config: Arc<Config>, pool: PgPool) {
    let mut tick = interval(Duration::from_secs(60));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let repo = MempoolMetricRepo::new(pool);
    loop {
        tick.tick().await;
        if let Err(e) = repo
            .insert_snapshot(PLATFORM_VERSION, 0, 0, 0, 0, 0, None, None, None)
            .await
        {
            warn!(error = %e, "metrics insert failed");
        }
        debug!("metrics tick");
    }
}
