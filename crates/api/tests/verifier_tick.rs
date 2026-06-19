//! Integration test for the verifier pipeline.
//!
//! Wiremock fakes the Soroban JSON-RPC endpoint. Tests insert UNVERIFIED transactions +
//! linked bundles, run one verifier tick, then assert the DB state transitioned correctly
//! AND that an event was broadcast on the EventBroadcaster channel.

mod common;

use chrono::{Duration, Utc};
use common::TestDb;
use provider_stack_core::{
    config::{Config, MempoolConfig},
    events::EventBroadcaster,
    pipelines::verifier::run_tick,
};
use provider_stack_persistence::{
    BundleStatus, BundleTransactionRepo, OperationsBundleRepo, TransactionRepo,
};
use serde_json::{json, Value};
use soroban_client::{Options, Server};
use std::sync::Arc;
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

/// Minimal config for the verifier tick — only the mempool op weights are read
/// (to summarize completed bundles).
fn cfg() -> Arc<Config> {
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
            slot_capacity: 10,
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

/// Seed a bundle in PROCESSING with one linked UNVERIFIED transaction.
async fn seed_bundle_with_tx(db: &TestDb, bundle_id: &str, tx_hash: &str) {
    let bundle_repo = OperationsBundleRepo::new(db.pool.clone());
    let tx_repo = TransactionRepo::new(db.pool.clone());
    let link = BundleTransactionRepo::new(db.pool.clone());

    let now = Utc::now();
    bundle_repo
        .create(
            bundle_id,
            now + Duration::hours(1),
            &json!([]),
            0,
            None,
            Some("test"),
        )
        .await
        .unwrap();
    bundle_repo
        .set_status(bundle_id, BundleStatus::Processing)
        .await
        .unwrap();
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
    let Some(db) = skip_if_no_db().await else {
        return;
    };

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

    let server = Server::new(
        &rpc.uri(),
        Options {
            allow_http: true,
            ..Options::default()
        },
    )
    .expect("Server::new");
    let config = cfg();
    run_tick(&server, &db.pool, &events, &config)
        .await
        .expect("verifier tick");

    // Transaction now VERIFIED.
    let tx_status: String =
        sqlx::query_scalar("SELECT status::text FROM transactions WHERE id = $1")
            .bind("TXHASH")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(tx_status, "VERIFIED");

    // Bundle now COMPLETED.
    let bundle_status: String =
        sqlx::query_scalar("SELECT status::text FROM operations_bundles WHERE id = $1")
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
    assert_eq!(ev.kind, "verifier.bundle_completed");
    assert_eq!(ev.payload["bundleIds"][0], "BUNDLE-1");
    assert_eq!(ev.payload["txId"], "TXHASH");

    db.cleanup().await;
}

#[actix_web::test]
async fn failed_marks_tx_failed_and_bundle_failed_and_broadcasts_event() {
    let Some(db) = skip_if_no_db().await else {
        return;
    };

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

    let server = Server::new(
        &rpc.uri(),
        Options {
            allow_http: true,
            ..Options::default()
        },
    )
    .expect("Server::new");
    let config = cfg();
    run_tick(&server, &db.pool, &events, &config)
        .await
        .expect("verifier tick");

    let tx_status: String =
        sqlx::query_scalar("SELECT status::text FROM transactions WHERE id = $1")
            .bind("TXHASH")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(tx_status, "FAILED");

    let bundle_status: String =
        sqlx::query_scalar("SELECT status::text FROM operations_bundles WHERE id = $1")
            .bind("BUNDLE-2")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(bundle_status, "FAILED");

    let ev = tokio::time::timeout(StdDuration::from_millis(100), rx.recv())
        .await
        .expect("event timeout")
        .expect("recv");
    assert_eq!(ev.kind, "verifier.bundle_failed");

    db.cleanup().await;
}

#[actix_web::test]
async fn not_found_leaves_tx_unverified() {
    let Some(db) = skip_if_no_db().await else {
        return;
    };

    let rpc = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(getx_response("NOT_FOUND", "TXHASH")),
        )
        .mount(&rpc)
        .await;

    seed_bundle_with_tx(&db, "BUNDLE-3", "TXHASH").await;

    let events = EventBroadcaster::new(256, "GTESTPP".to_string());
    let server = Server::new(
        &rpc.uri(),
        Options {
            allow_http: true,
            ..Options::default()
        },
    )
    .expect("Server::new");
    let config = cfg();
    run_tick(&server, &db.pool, &events, &config)
        .await
        .expect("verifier tick");

    let tx_status: String =
        sqlx::query_scalar("SELECT status::text FROM transactions WHERE id = $1")
            .bind("TXHASH")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        tx_status, "UNVERIFIED",
        "NOT_FOUND should leave tx untouched for retry"
    );

    let bundle_status: String =
        sqlx::query_scalar("SELECT status::text FROM operations_bundles WHERE id = $1")
            .bind("BUNDLE-3")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(bundle_status, "PROCESSING", "bundle untouched on NOT_FOUND");

    db.cleanup().await;
}
