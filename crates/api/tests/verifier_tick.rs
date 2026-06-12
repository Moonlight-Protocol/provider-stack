//! Integration test for the verifier pipeline.
//!
//! Wiremock fakes the Soroban JSON-RPC endpoint. Tests insert UNVERIFIED transactions +
//! linked bundles, run one verifier tick, then assert the DB state transitioned correctly
//! AND that an event was broadcast on the EventBroadcaster channel.

mod common;

use chrono::{Duration, Utc};
use common::TestDb;
use provider_stack_core::{
    events::{EventBroadcaster, ProviderEvent},
    pipelines::verifier::run_tick,
};
use provider_stack_persistence::{
    BundleStatus, BundleTransactionRepo, OperationsBundleRepo, TransactionRepo, TransactionStatus,
};
use serde_json::{json, Value};
use soroban_client::{Options, Server};
use std::time::Duration as StdDuration;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn skip_if_no_db() -> Option<TestDb> {
    if std::env::var("DATABASE_URL").is_err() {
        eprintln!("DATABASE_URL not set — skipping verifier integration test");
        return None;
    }
    Some(TestDb::create().await)
}

/// Seed a bundle in PROCESSING with one linked UNVERIFIED transaction.
async fn seed_bundle_with_tx(
    db: &TestDb,
    bundle_id: &str,
    tx_hash: &str,
) {
    let bundle_repo = OperationsBundleRepo::new(db.pool.clone());
    let tx_repo = TransactionRepo::new(db.pool.clone());
    let link = BundleTransactionRepo::new(db.pool.clone());

    let now = Utc::now();
    bundle_repo
        .create(bundle_id, now + Duration::hours(1), &json!([]), 0, None, Some("test"))
        .await
        .unwrap();
    bundle_repo.set_status(bundle_id, BundleStatus::Processing).await.unwrap();
    tx_repo
        .create(tx_hash, now + Duration::hours(1), "12345")
        .await
        .unwrap();
    link.link(bundle_id, tx_hash).await.unwrap();
}

/// JSON-RPC body the verifier sends for getTransaction.
fn getx_response(status: &str, tx_hash: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "latestLedger": 12_345,
            "latestLedgerCloseTime": "1700000000",
            "oldestLedger": 1,
            "oldestLedgerCloseTime": "1600000000",
            "createdAt": "1700000050",
            "status": status,
            "txHash": tx_hash,
            "ledger": 12_345,
            "applicationOrder": 0,
            "feeBump": false
        }
    })
}

#[actix_web::test]
async fn success_marks_tx_verified_and_bundle_completed_and_broadcasts_event() {
    let Some(db) = skip_if_no_db().await else { return; };

    let rpc = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_partial_json(json!({ "method": "getTransaction" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(getx_response("SUCCESS", "TXHASH")))
        .mount(&rpc)
        .await;

    seed_bundle_with_tx(&db, "BUNDLE-1", "TXHASH").await;

    let events = EventBroadcaster::new(256, "GTESTPP".to_string());
    let mut rx = events.subscribe();

    let server = Server::new(&rpc.uri(), Options { allow_http: true, ..Options::default() })
        .expect("Server::new");
    run_tick(&server, &db.pool, &events).await.expect("verifier tick");

    // Transaction now VERIFIED.
    let tx_status: String = sqlx::query_scalar(
        "SELECT status::text FROM transactions WHERE id = $1",
    )
    .bind("TXHASH")
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(tx_status, "VERIFIED");

    // Bundle now COMPLETED.
    let bundle_status: String = sqlx::query_scalar(
        "SELECT status::text FROM operations_bundles WHERE id = $1",
    )
    .bind("BUNDLE-1")
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(bundle_status, "COMPLETED");

    // BundleCompleted event broadcast.
    let ev = tokio::time::timeout(StdDuration::from_millis(100), rx.recv())
        .await
        .expect("event timeout")
        .expect("recv");
    match ev {
        ProviderEvent::BundleCompleted { bundle_id, tx_hash } => {
            assert_eq!(bundle_id, "BUNDLE-1");
            assert_eq!(tx_hash, "TXHASH");
        }
        other => panic!("expected BundleCompleted, got {other:?}"),
    }

    db.cleanup().await;
}

#[actix_web::test]
async fn failed_marks_tx_failed_and_bundle_failed_and_broadcasts_event() {
    let Some(db) = skip_if_no_db().await else { return; };

    let rpc = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_partial_json(json!({ "method": "getTransaction" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(getx_response("FAILED", "TXHASH")))
        .mount(&rpc)
        .await;

    seed_bundle_with_tx(&db, "BUNDLE-2", "TXHASH").await;

    let events = EventBroadcaster::new(256, "GTESTPP".to_string());
    let mut rx = events.subscribe();

    let server = Server::new(&rpc.uri(), Options { allow_http: true, ..Options::default() })
        .expect("Server::new");
    run_tick(&server, &db.pool, &events).await.expect("verifier tick");

    let tx_status: String = sqlx::query_scalar(
        "SELECT status::text FROM transactions WHERE id = $1",
    )
    .bind("TXHASH")
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(tx_status, "FAILED");

    let bundle_status: String = sqlx::query_scalar(
        "SELECT status::text FROM operations_bundles WHERE id = $1",
    )
    .bind("BUNDLE-2")
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(bundle_status, "FAILED");

    let ev = tokio::time::timeout(StdDuration::from_millis(100), rx.recv())
        .await
        .expect("event timeout")
        .expect("recv");
    assert!(matches!(ev, ProviderEvent::BundleFailed { .. }));

    db.cleanup().await;
}

#[actix_web::test]
async fn not_found_leaves_tx_unverified() {
    let Some(db) = skip_if_no_db().await else { return; };

    let rpc = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(getx_response("NOT_FOUND", "TXHASH")))
        .mount(&rpc)
        .await;

    seed_bundle_with_tx(&db, "BUNDLE-3", "TXHASH").await;

    let events = EventBroadcaster::new(256, "GTESTPP".to_string());
    let server = Server::new(&rpc.uri(), Options { allow_http: true, ..Options::default() })
        .expect("Server::new");
    run_tick(&server, &db.pool, &events).await.expect("verifier tick");

    let tx_status: String = sqlx::query_scalar(
        "SELECT status::text FROM transactions WHERE id = $1",
    )
    .bind("TXHASH")
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(tx_status, "UNVERIFIED", "NOT_FOUND should leave tx untouched for retry");

    let bundle_status: String = sqlx::query_scalar(
        "SELECT status::text FROM operations_bundles WHERE id = $1",
    )
    .bind("BUNDLE-3")
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(bundle_status, "PROCESSING", "bundle untouched on NOT_FOUND");

    db.cleanup().await;
}
