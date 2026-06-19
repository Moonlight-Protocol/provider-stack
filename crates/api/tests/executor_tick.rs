//! Integration test for the executor pipeline.
//!
//! Wiremock fakes Soroban JSON-RPC:
//!   getLedgerEntries → returns a minimal AccountEntry XDR for the PP account.
//!   sendTransaction   → returns hash + status PENDING.
//!
//! Inserts a PROCESSING bundle, runs one executor tick, asserts:
//!   - a transactions row is inserted with the returned hash,
//!   - a bundles_transactions row links bundle → tx.

mod common;

use chrono::{Duration, Utc};
use common::TestDb;
use ed25519_dalek::SigningKey;
use provider_stack_core::{
    config::{Config, MempoolConfig},
    events::EventBroadcaster,
    pipelines::executor::run_tick,
};
use provider_stack_persistence::{BundleStatus, OperationsBundleRepo};
use serde_json::{json, Value};
use soroban_client::{Options, Server};
use sqlx::Row;
use std::sync::Arc;
use std::time::Duration as StdDuration;
// IMPORTANT: there are multiple stellar-xdr versions in the graph (v25/v26/v27). soroban-client
// internally uses v26 via stellar-baselib. To construct values that soroban-client will accept,
// use the re-exports under soroban_client::xdr, NOT the top-level stellar_xdr crate.
use soroban_client::xdr::{
    AccountEntry, AccountEntryExt, AccountId, LedgerEntryData, LedgerKey, LedgerKeyAccount, Limits,
    PublicKey, SequenceNumber, String32, StringM, Thresholds, Uint256, VecM, WriteXdr,
};
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn skip_if_no_db() -> Option<TestDb> {
    if std::env::var("DATABASE_URL").is_err() {
        eprintln!("DATABASE_URL not set — skipping executor integration test");
        return None;
    }
    Some(TestDb::create().await)
}

fn pp_keypair_seed() -> [u8; 32] {
    [0xABu8; 32]
}

fn pp_strkey_secret() -> String {
    format!("{}", stellar_strkey::ed25519::PrivateKey(pp_keypair_seed()))
}

/// Build a base64 LedgerEntryData::Account XDR for the PP account with a fixed seq num.
fn account_entry_data_b64(pp_pubkey_bytes: [u8; 32], seq: i64) -> String {
    let entry = AccountEntry {
        account_id: AccountId(PublicKey::PublicKeyTypeEd25519(Uint256(pp_pubkey_bytes))),
        balance: 100_000_000_000,
        seq_num: SequenceNumber(seq),
        num_sub_entries: 0,
        inflation_dest: None,
        flags: 0,
        home_domain: String32(StringM::default()),
        thresholds: Thresholds([1, 0, 0, 0]),
        signers: VecM::default(),
        ext: AccountEntryExt::V0,
    };
    let data = LedgerEntryData::Account(entry);
    data.to_xdr_base64(Limits::none())
        .expect("encode account entry data")
}

fn ledger_entries_response(pp_pubkey_bytes: [u8; 32]) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "entries": [{
                "lastModifiedLedgerSeq": 100,
                "liveUntilLedgerSeq": 200,
                "key": LedgerKey::Account(LedgerKeyAccount {
                    account_id: AccountId(PublicKey::PublicKeyTypeEd25519(Uint256(pp_pubkey_bytes))),
                }).to_xdr_base64(Limits::none()).unwrap(),
                "xdr": account_entry_data_b64(pp_pubkey_bytes, 42)
            }],
            "latestLedger": 1234u32
        }
    })
}

fn send_tx_response(hash: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "status": "PENDING",
            "hash": hash,
            "latestLedger": 1234u32,
            "latestLedgerCloseTime": "1700000000"
        }
    })
}

fn cfg(rpc_url: &str) -> Arc<Config> {
    Arc::new(Config {
        port: 0,
        mode: "test".into(),
        log_level: "warn".into(),
        database_url: String::new(),
        network: "standalone".into(),
        network_fee: 1_000_000,
        stellar_rpc_url: rpc_url.into(),
        transaction_expiration_offset: 1_000,
        event_watcher_interval: StdDuration::from_millis(30_000),
        service_domain: "smoke.local".into(),
        service_auth_secret: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".into(),
        provider_base_url: "http://localhost:3010".into(),
        operator_public_key: String::new(),
        pp_secret_key: pp_strkey_secret(),
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

#[actix_web::test]
async fn processing_bundle_submitted_and_tx_row_recorded() {
    let Some(db) = skip_if_no_db().await else {
        return;
    };

    let rpc = MockServer::start().await;
    let pp_pubkey_bytes = SigningKey::from_bytes(&pp_keypair_seed())
        .verifying_key()
        .to_bytes();

    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_partial_json(json!({ "method": "getLedgerEntries" })))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(ledger_entries_response(pp_pubkey_bytes)),
        )
        .mount(&rpc)
        .await;

    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_partial_json(json!({ "method": "sendTransaction" })))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(send_tx_response("TX_HASH_FROM_RPC")),
        )
        .mount(&rpc)
        .await;

    let bundles_repo = OperationsBundleRepo::new(db.pool.clone());
    let now = Utc::now();
    // Channel contract id — valid Stellar contract strkey (C-prefixed, checksummed).
    let channel_id_owned = format!("{}", stellar_strkey::Contract([0x11u8; 32]));
    let channel_id = channel_id_owned.as_str();

    bundles_repo
        .create(
            "BUNDLE-EXEC-1",
            now + Duration::hours(1),
            &json!([]), // empty operations — channel.transact([]) is the cheapest happy path
            0,
            Some(channel_id),
            Some("test"),
        )
        .await
        .unwrap();
    bundles_repo
        .set_status("BUNDLE-EXEC-1", BundleStatus::Processing)
        .await
        .unwrap();

    let config = cfg(&rpc.uri());
    let server = Server::new(
        &rpc.uri(),
        Options {
            allow_http: true,
            ..Options::default()
        },
    )
    .unwrap();
    let events = EventBroadcaster::new(256, "GTESTPP".to_string());
    run_tick(&server, &db.pool, &config, &events)
        .await
        .expect("executor tick");

    // transactions row inserted with the RPC-returned hash.
    let row = sqlx::query("SELECT id, status::text FROM transactions WHERE id = $1")
        .bind("TX_HASH_FROM_RPC")
        .fetch_one(&db.pool)
        .await
        .expect("transactions row");
    assert_eq!(row.get::<String, _>("id"), "TX_HASH_FROM_RPC");
    assert_eq!(row.get::<String, _>("status"), "UNVERIFIED");

    // bundles_transactions link in place.
    let link_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM bundles_transactions WHERE bundle_id = $1 AND transaction_id = $2",
    )
    .bind("BUNDLE-EXEC-1")
    .bind("TX_HASH_FROM_RPC")
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(link_count, 1);

    db.cleanup().await;
}

#[actix_web::test]
async fn executor_skips_when_no_processing_bundles() {
    let Some(db) = skip_if_no_db().await else {
        return;
    };
    // No mocks needed — executor should make zero RPC calls.
    let rpc = MockServer::start().await;

    let config = cfg(&rpc.uri());
    let server = Server::new(
        &rpc.uri(),
        Options {
            allow_http: true,
            ..Options::default()
        },
    )
    .unwrap();
    let events = EventBroadcaster::new(256, "GTESTPP".to_string());
    run_tick(&server, &db.pool, &config, &events)
        .await
        .expect("executor tick on empty");

    let tx_count: i64 = sqlx::query_scalar("SELECT count(*) FROM transactions")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(tx_count, 0);

    db.cleanup().await;
}
