//! Integration test for the metrics collector: insert bundles in known statuses,
//! call `snapshot()`, assert the persisted `mempool_metrics` row matches.

mod common;

use chrono::{Duration, Utc};
use common::TestDb;
use provider_stack_core::pipelines::metrics::snapshot;
use provider_stack_persistence::{BundleStatus, OperationsBundleRepo};
use serde_json::json;
use sqlx::Row;

async fn skip_if_no_db() -> Option<TestDb> {
    if std::env::var("DATABASE_URL").is_err() {
        eprintln!("DATABASE_URL not set — skipping metrics integration test");
        return None;
    }
    Some(TestDb::create().await)
}

async fn seed_bundles(repo: &OperationsBundleRepo) {
    let now = Utc::now();
    // 2 PENDING (in future)
    for i in 0..2 {
        repo.create(
            &format!("pending-{i}"),
            now + Duration::hours(1),
            &json!([]),
            0,
            None,
            Some("test"),
        )
        .await
        .unwrap();
    }
    // 1 PROCESSING
    repo.create(
        "processing-1",
        now + Duration::hours(1),
        &json!([]),
        0,
        None,
        Some("test"),
    )
    .await
    .unwrap();
    repo.set_status("processing-1", BundleStatus::Processing)
        .await
        .unwrap();
    // 3 COMPLETED
    for i in 0..3 {
        repo.create(
            &format!("completed-{i}"),
            now + Duration::hours(1),
            &json!([]),
            0,
            None,
            Some("test"),
        )
        .await
        .unwrap();
        repo.set_status(&format!("completed-{i}"), BundleStatus::Completed)
            .await
            .unwrap();
    }
    // 1 EXPIRED
    repo.create(
        "expired-1",
        now - Duration::hours(1),
        &json!([]),
        0,
        None,
        Some("test"),
    )
    .await
    .unwrap();
    repo.set_status("expired-1", BundleStatus::Expired)
        .await
        .unwrap();
    // 1 FAILED
    repo.create(
        "failed-1",
        now + Duration::hours(1),
        &json!([]),
        0,
        None,
        Some("test"),
    )
    .await
    .unwrap();
    repo.set_status("failed-1", BundleStatus::Failed)
        .await
        .unwrap();
}

#[actix_web::test]
async fn snapshot_records_per_status_counts() {
    let Some(db) = skip_if_no_db().await else {
        return;
    };
    let repo = OperationsBundleRepo::new(db.pool.clone());
    seed_bundles(&repo).await;

    snapshot(&db.pool).await.expect("snapshot");

    let row = sqlx::query(
        r#"SELECT queue_depth, slot_count, bundles_completed, bundles_expired, bundles_failed
           FROM mempool_metrics ORDER BY recorded_at DESC LIMIT 1"#,
    )
    .fetch_one(&db.pool)
    .await
    .expect("mempool_metrics row");

    assert_eq!(row.get::<i32, _>("queue_depth"), 2);
    assert_eq!(row.get::<i32, _>("slot_count"), 1);
    assert_eq!(row.get::<i32, _>("bundles_completed"), 3);
    assert_eq!(row.get::<i32, _>("bundles_expired"), 1);
    assert_eq!(row.get::<i32, _>("bundles_failed"), 1);

    db.cleanup().await;
}

#[actix_web::test]
async fn snapshot_with_empty_db_writes_zeros() {
    let Some(db) = skip_if_no_db().await else {
        return;
    };

    snapshot(&db.pool).await.expect("snapshot");

    let row = sqlx::query(
        r#"SELECT queue_depth, slot_count, bundles_completed, bundles_expired, bundles_failed,
                  avg_processing_ms, p95_processing_ms, throughput_per_min
           FROM mempool_metrics ORDER BY recorded_at DESC LIMIT 1"#,
    )
    .fetch_one(&db.pool)
    .await
    .expect("mempool_metrics row");

    assert_eq!(row.get::<i32, _>("queue_depth"), 0);
    assert_eq!(row.get::<i32, _>("slot_count"), 0);
    assert_eq!(row.get::<i32, _>("bundles_completed"), 0);
    assert!(row.get::<Option<f64>, _>("avg_processing_ms").is_none());
    assert!(row.get::<Option<f64>, _>("throughput_per_min").is_none());

    db.cleanup().await;
}
