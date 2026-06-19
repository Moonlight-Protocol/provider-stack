//! Integration test for the event watcher.
//!
//! Wiremock fakes Soroban `getHealth` + `getEvents`. Tests insert a PENDING
//! council_memberships row and run one watcher tick:
//!  - On a `provider_added` event: membership transitions to ACTIVE.
//!  - On a `provider_removed` event: membership transitions to REJECTED.
//!  - On no events: membership stays as-is.
//!
//! The cursor is in-memory only (no durable store): a fresh map seeds the first
//! poll from the RPC's oldest available ledger.

mod common;

use common::TestDb;
use provider_stack_core::events::EventBroadcaster;
use provider_stack_core::pipelines::event_watcher::run_tick;
use provider_stack_persistence::{CouncilMembershipRepo, CouncilMembershipStatus};
use serde_json::{json, Value};
use soroban_client::{Options, Server};
use std::collections::HashMap;
use stellar_xdr::{Limits, ScSymbol, ScVal, WriteXdr};
use uuid::Uuid;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn skip_if_no_db() -> Option<TestDb> {
    if std::env::var("DATABASE_URL").is_err() {
        eprintln!("DATABASE_URL not set — skipping event-watcher integration test");
        return None;
    }
    Some(TestDb::create().await)
}

const CHANNEL_AUTH_ID: &str = "CABCXYZ123";

async fn seed_pending_membership(db: &TestDb) -> String {
    let repo = CouncilMembershipRepo::new(db.pool.clone());
    let id = Uuid::new_v4().to_string();
    repo.create(
        &id,
        "https://council.example",
        "GCOUNCIL",
        CHANNEL_AUTH_ID,
        Some("[\"US\"]"),
    )
    .await
    .expect("seed membership");
    id
}

/// Encode a symbol topic as base64 ScVal XDR (what RPC returns).
fn topic_b64(name: &str) -> String {
    let sym = ScVal::Symbol(ScSymbol(name.as_bytes().to_vec().try_into().unwrap()));
    sym.to_xdr_base64(Limits::none()).expect("xdr encode")
}

fn events_response(topic: &str, cursor: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "events": [{
                "type": "contract",
                "ledger": 1000,
                "ledgerClosedAt": "2026-01-01T00:00:00Z",
                "contractId": CHANNEL_AUTH_ID,
                "id": "1234-1",
                "pagingToken": "1234-1",
                "inSuccessfulContractCall": true,
                "txHash": "TXHASH",
                "topic": [topic],
                "value": topic
            }],
            "cursor": cursor,
            "latestLedger": 1100u64,
            "oldestLedger": 1u64,
            "latestLedgerCloseTime": "2026-01-01T00:00:00Z",
            "oldestLedgerCloseTime": "2024-01-01T00:00:00Z"
        }
    })
}

fn empty_events_response(cursor: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "events": [],
            "cursor": cursor,
            "latestLedger": 1100u64,
            "oldestLedger": 1u64,
            "latestLedgerCloseTime": "2026-01-01T00:00:00Z",
            "oldestLedgerCloseTime": "2024-01-01T00:00:00Z"
        }
    })
}

/// The watcher seeds a fresh (cursorless) poll from the RPC's oldest available
/// ledger, so it calls `getHealth` first. Mount this alongside the getEvents
/// mock on every test's mock server.
async fn mount_health(rpc: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_partial_json(json!({ "method": "getHealth" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "status": "healthy",
                "latestLedger": 1100u64,
                "oldestLedger": 1u64,
                "ledgerRetentionWindow": 1100u64
            }
        })))
        .mount(rpc)
        .await;
}

#[actix_web::test]
async fn provider_added_event_promotes_membership_to_active() {
    let Some(db) = skip_if_no_db().await else { return; };
    seed_pending_membership(&db).await;

    let rpc = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_partial_json(json!({ "method": "getEvents" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(events_response(
            &topic_b64("provider_added"),
            "cursor-1",
        )))
        .mount(&rpc)
        .await;
    mount_health(&rpc).await;

    let server = Server::new(&rpc.uri(), Options { allow_http: true, ..Options::default() })
        .expect("Server::new");
    let events = EventBroadcaster::new(256, "GTESTPP".to_string());
    let mut cursors = HashMap::new();
    run_tick(&server, &db.pool, &events, &mut cursors).await.expect("watcher tick");

    let status: String = sqlx::query_scalar(
        "SELECT status::text FROM council_memberships WHERE channel_auth_id = $1",
    )
    .bind(CHANNEL_AUTH_ID)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(status, "ACTIVE");

    // Cursor advanced in memory (no durable store), so a subsequent tick would
    // resume after it rather than re-seed from oldest.
    assert_eq!(cursors.get(CHANNEL_AUTH_ID).map(String::as_str), Some("cursor-1"));

    db.cleanup().await;
}

#[actix_web::test]
async fn provider_removed_event_marks_membership_rejected() {
    let Some(db) = skip_if_no_db().await else { return; };
    seed_pending_membership(&db).await;
    // Pre-promote it to ACTIVE so the removed path is exercised.
    let repo = CouncilMembershipRepo::new(db.pool.clone());
    repo.set_status(CHANNEL_AUTH_ID, CouncilMembershipStatus::Active).await.unwrap();

    let rpc = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_partial_json(json!({ "method": "getEvents" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(events_response(
            &topic_b64("provider_removed"),
            "cursor-2",
        )))
        .mount(&rpc)
        .await;
    mount_health(&rpc).await;

    let server = Server::new(&rpc.uri(), Options { allow_http: true, ..Options::default() })
        .expect("Server::new");
    let events = EventBroadcaster::new(256, "GTESTPP".to_string());
    run_tick(&server, &db.pool, &events, &mut HashMap::new()).await.expect("watcher tick");

    let status: String = sqlx::query_scalar(
        "SELECT status::text FROM council_memberships WHERE channel_auth_id = $1",
    )
    .bind(CHANNEL_AUTH_ID)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(status, "REJECTED");

    db.cleanup().await;
}

#[actix_web::test]
async fn empty_events_response_leaves_membership_unchanged() {
    let Some(db) = skip_if_no_db().await else { return; };
    seed_pending_membership(&db).await;

    let rpc = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_partial_json(json!({ "method": "getEvents" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty_events_response("cursor-3")))
        .mount(&rpc)
        .await;
    mount_health(&rpc).await;

    let server = Server::new(&rpc.uri(), Options { allow_http: true, ..Options::default() })
        .expect("Server::new");
    let events = EventBroadcaster::new(256, "GTESTPP".to_string());
    run_tick(&server, &db.pool, &events, &mut HashMap::new()).await.expect("watcher tick");

    let status: String = sqlx::query_scalar(
        "SELECT status::text FROM council_memberships WHERE channel_auth_id = $1",
    )
    .bind(CHANNEL_AUTH_ID)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(status, "PENDING", "no events → status unchanged");

    db.cleanup().await;
}
