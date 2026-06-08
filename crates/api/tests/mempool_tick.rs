//! Integration test for the mempool processor's tick logic — directly drives `run_tick`
//! against a real Postgres so the SQL paths run end-to-end. Independent of HTTP.

mod common;

use chrono::Duration;
use common::TestDb;
use provider_stack_core::{
    config::{Config, MempoolConfig},
    pipelines::mempool::run_tick,
};
use provider_stack_persistence::{BundleStatus, OperationsBundleRepo};
use serde_json::json;
use sqlx::Row;
use std::sync::Arc;
use std::time::Duration as StdDuration;

async fn skip_if_no_db() -> Option<TestDb> {
    if std::env::var("DATABASE_URL").is_err() {
        eprintln!("DATABASE_URL not set — skipping mempool integration test");
        return None;
    }
    Some(TestDb::create().await)
}

fn cfg_with_capacity(capacity: usize) -> Arc<Config> {
    Arc::new(Config {
        port: 0,
        mode: "test".into(),
        log_level: "warn".into(),
        database_url: String::new(),
        network: "standalone".into(),
        network_fee: 1_000_000,
        stellar_rpc_url: String::new(),
        transaction_expiration_offset: 1_000,
        event_watcher_interval: StdDuration::from_millis(30_000),
        service_domain: "smoke.local".into(),
        service_auth_secret: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".into(),
        provider_base_url: "http://localhost:3010".into(),
        operator_public_key: String::new(),
        pp_secret_key: String::new(),
        challenge_ttl: StdDuration::from_secs(900),
        session_ttl: StdDuration::from_secs(21_600),
        mempool: MempoolConfig {
            slot_capacity: capacity,
            expensive_op_weight: 10,
            cheap_op_weight: 1,
            executor_interval: StdDuration::from_millis(2_000),
            verifier_interval: StdDuration::from_millis(2_000),
            ttl_check_interval: StdDuration::from_millis(5_000),
            max_retry_attempts: 3,
            startup_max_bundle_age: StdDuration::ZERO,
        },
        bundle_max_operations: 200,
        allowed_origins: vec![],
    })
}

async fn insert_bundle(
    repo: &OperationsBundleRepo,
    id: &str,
    ttl: chrono::DateTime<chrono::Utc>,
) {
    repo.create(id, ttl, &json!([]), 0, None, Some("test"))
        .await
        .expect("insert bundle");
}

#[actix_web::test]
async fn tick_expires_past_ttl_and_promotes_up_to_capacity() {
    let Some(db) = skip_if_no_db().await else { return; };

    let repo = OperationsBundleRepo::new(db.pool.clone());

    let now = chrono::Utc::now();
    // 2 expired, 5 pending in future, capacity = 3.
    insert_bundle(&repo, "expired-1", now - Duration::minutes(1)).await;
    insert_bundle(&repo, "expired-2", now - Duration::seconds(30)).await;
    for i in 0..5 {
        insert_bundle(&repo, &format!("pending-{i}"), now + Duration::hours(1)).await;
    }

    let config = cfg_with_capacity(3);
    run_tick(&repo, &config).await.expect("tick");

    // Two expired bundles now EXPIRED.
    let expired_count: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM operations_bundles WHERE status = 'EXPIRED'::bundle_status"#,
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(expired_count, 2, "two bundles should be expired");

    // Three pending promoted to PROCESSING (capacity = 3).
    let processing_count: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM operations_bundles WHERE status = 'PROCESSING'::bundle_status"#,
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(processing_count, 3, "three bundles should be promoted to PROCESSING");

    // Remaining two stay PENDING.
    let pending_count: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM operations_bundles WHERE status = 'PENDING'::bundle_status"#,
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(pending_count, 2, "two bundles should remain PENDING");

    db.cleanup().await;
}

#[actix_web::test]
async fn tick_promotes_oldest_first() {
    let Some(db) = skip_if_no_db().await else { return; };
    let repo = OperationsBundleRepo::new(db.pool.clone());
    let now = chrono::Utc::now();

    // Insert three pending bundles, each with a slight created_at delay.
    insert_bundle(&repo, "first", now + Duration::hours(1)).await;
    tokio::time::sleep(StdDuration::from_millis(50)).await;
    insert_bundle(&repo, "second", now + Duration::hours(1)).await;
    tokio::time::sleep(StdDuration::from_millis(50)).await;
    insert_bundle(&repo, "third", now + Duration::hours(1)).await;

    let config = cfg_with_capacity(1);
    run_tick(&repo, &config).await.expect("tick");

    let row = sqlx::query(
        r#"SELECT id FROM operations_bundles WHERE status = 'PROCESSING'::bundle_status"#,
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let id: String = row.get("id");
    assert_eq!(id, "first", "oldest bundle should be promoted first");

    db.cleanup().await;
}

#[actix_web::test]
async fn tick_is_idempotent_when_full() {
    let Some(db) = skip_if_no_db().await else { return; };
    let repo = OperationsBundleRepo::new(db.pool.clone());
    let now = chrono::Utc::now();

    for i in 0..3 {
        insert_bundle(&repo, &format!("b-{i}"), now + Duration::hours(1)).await;
    }
    let config = cfg_with_capacity(3);

    // First tick promotes all 3.
    run_tick(&repo, &config).await.unwrap();

    // Snapshot statuses.
    let snap_before: Vec<(String, String)> = sqlx::query_as(
        r#"SELECT id, status::text FROM operations_bundles ORDER BY id"#,
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();

    // Second tick should be a no-op (slots full).
    run_tick(&repo, &config).await.unwrap();

    let snap_after: Vec<(String, String)> = sqlx::query_as(
        r#"SELECT id, status::text FROM operations_bundles ORDER BY id"#,
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();

    assert_eq!(snap_before, snap_after, "second tick should not change state when full");

    db.cleanup().await;
}
