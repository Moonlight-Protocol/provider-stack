//! Metrics collector — snapshot every 60s into `mempool_metrics`.
//!
//! Real numbers (queue depth, slot count, completion totals) come straight from the bundle
//! status counts. Per-bundle latency aggregates (avg/p95) are computed from completed bundles'
//! `updated_at - created_at`.

use crate::config::Config;
use provider_stack_persistence::{MempoolMetricRepo, PgPool};
use sqlx::Row;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::{interval, MissedTickBehavior};
use tracing::{debug, instrument, warn};

const PLATFORM_VERSION: &str = env!("CARGO_PKG_VERSION");
const SNAPSHOT_INTERVAL: Duration = Duration::from_secs(60);
const LATENCY_WINDOW_MINUTES: i64 = 60;

#[instrument(skip_all, name = "pipeline.metrics")]
pub async fn run(_config: Arc<Config>, pool: PgPool) {
    let mut tick = interval(SNAPSHOT_INTERVAL);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        if let Err(e) = snapshot(&pool).await {
            warn!(error = %e, "metrics snapshot failed");
        }
        debug!("metrics tick complete");
    }
}

/// Compute + persist one snapshot. Exposed for the integration test.
pub async fn snapshot(pool: &PgPool) -> anyhow::Result<()> {
    let queue_depth: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM operations_bundles
           WHERE status = 'PENDING'::bundle_status AND deleted_at IS NULL"#,
    )
    .fetch_one(pool)
    .await?;

    let slot_count: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM operations_bundles
           WHERE status = 'PROCESSING'::bundle_status AND deleted_at IS NULL"#,
    )
    .fetch_one(pool)
    .await?;

    let bundles_completed: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM operations_bundles
           WHERE status = 'COMPLETED'::bundle_status AND deleted_at IS NULL"#,
    )
    .fetch_one(pool)
    .await?;

    let bundles_expired: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM operations_bundles
           WHERE status = 'EXPIRED'::bundle_status AND deleted_at IS NULL"#,
    )
    .fetch_one(pool)
    .await?;

    let bundles_failed: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM operations_bundles
           WHERE status = 'FAILED'::bundle_status AND deleted_at IS NULL"#,
    )
    .fetch_one(pool)
    .await?;

    // Latency aggregates: bundles completed in the last LATENCY_WINDOW_MINUTES.
    let lat_row = sqlx::query(
        r#"SELECT
              extract(epoch from avg(updated_at - created_at)) * 1000 as avg_ms,
              extract(epoch from percentile_disc(0.95) WITHIN GROUP (ORDER BY (updated_at - created_at))) * 1000 as p95_ms,
              count(*) as completed_in_window
           FROM operations_bundles
           WHERE status = 'COMPLETED'::bundle_status
             AND deleted_at IS NULL
             AND updated_at > now() - ($1::int || ' minutes')::interval"#,
    )
    .bind(LATENCY_WINDOW_MINUTES as i32)
    .fetch_one(pool)
    .await?;
    let avg_ms: Option<f64> = lat_row.try_get("avg_ms").unwrap_or(None);
    let p95_ms: Option<f64> = lat_row.try_get("p95_ms").unwrap_or(None);
    let completed_in_window: i64 = lat_row.try_get("completed_in_window").unwrap_or(0);
    let throughput_per_min = if completed_in_window > 0 {
        Some(completed_in_window as f64 / LATENCY_WINDOW_MINUTES as f64)
    } else {
        None
    };

    let repo = MempoolMetricRepo::new(pool.clone());
    repo.insert_snapshot(
        PLATFORM_VERSION,
        queue_depth as i32,
        slot_count as i32,
        bundles_completed as i32,
        bundles_expired as i32,
        bundles_failed as i32,
        avg_ms,
        p95_ms,
        throughput_per_min,
    )
    .await?;
    Ok(())
}
