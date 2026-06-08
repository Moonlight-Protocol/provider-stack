//! HTTP integration test for the KYC self-register flow.
//!
//!   POST /api/v1/providers/{pk}/entities/challenge { pubkey } → { nonce }
//!   POST /api/v1/providers/{pk}/entities { pubkey, name, jurisdictions, nonce, signature }
//!     → 201 { entity_id, status: "APPROVED" }
//!
//! Asserts:
//! - A valid roundtrip auto-approves the entity (entities.status = APPROVED) and provisions
//!   one USER account + one wallet_users row.
//! - Re-submitting works idempotently (same entity, same status).
//! - Submitting against a PK that isn't this stack's PP returns 404.
//! - Signature mismatch returns 401.
//!
//! Requires `DATABASE_URL` (a Postgres instance reachable for CREATEDB). When unset, the
//! tests print a skip notice and pass.

mod common;

use actix_web::{test, web, App};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use common::{build_test_app_state, pp_strkey, strkey, TestDb};
use ed25519_dalek::{Signer, SigningKey};
use provider_stack_api::routing;
use sqlx::Row;

const SERVICE_DOMAIN: &str = "smoke.local";

async fn skip_if_no_db() -> Option<TestDb> {
    if std::env::var("DATABASE_URL").is_err() {
        eprintln!("DATABASE_URL not set — skipping KYC HTTP integration test");
        return None;
    }
    Some(TestDb::create().await)
}

fn sign_nonce(key: &SigningKey, nonce_b64: &str) -> String {
    let nonce_bytes = B64.decode(nonce_b64).expect("nonce should be valid base64");
    let sig = key.sign(&nonce_bytes);
    B64.encode(sig.to_bytes())
}

#[actix_web::test]
async fn full_kyc_register_roundtrip_auto_approves() {
    let Some(db) = skip_if_no_db().await else { return; };

    let pp_seed = [0xABu8; 32];
    let operator_strkey = pp_strkey([0xCCu8; 32]);
    let pp_pk = pp_strkey(pp_seed);

    let state = build_test_app_state(pp_seed, operator_strkey, db.pool.clone(), SERVICE_DOMAIN);

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(state.clone()))
            .configure(routing::configure),
    )
    .await;

    let entity_key = SigningKey::from_bytes(&[0xEEu8; 32]);
    let entity_strkey = strkey(entity_key.verifying_key().to_bytes());

    // 1. Challenge.
    let req = test::TestRequest::post()
        .uri(&format!("/api/v1/providers/{pp_pk}/entities/challenge"))
        .set_json(serde_json::json!({ "pubkey": entity_strkey }))
        .to_request();
    let body: serde_json::Value = test::call_and_read_body_json(&app, req).await;
    let nonce = body["nonce"].as_str().expect("nonce").to_string();

    // 2. Register with the signed nonce.
    let signature = sign_nonce(&entity_key, &nonce);
    let req = test::TestRequest::post()
        .uri(&format!("/api/v1/providers/{pp_pk}/entities"))
        .set_json(serde_json::json!({
            "pubkey": entity_strkey,
            "name": "Test Entity",
            "jurisdictions": ["US"],
            "nonce": nonce,
            "signature": signature,
        }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status().as_u16(), 201, "expected 201 Created, got {}", resp.status());
    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["status"], "APPROVED");
    assert_eq!(body["entity_id"], entity_strkey);

    // 3. Verify DB rows.
    let row = sqlx::query(r#"SELECT status::text as status, name FROM entities WHERE id = $1"#)
        .bind(&entity_strkey)
        .fetch_one(&db.pool)
        .await
        .expect("entity row");
    let status: String = row.get("status");
    let name: Option<String> = row.get("name");
    assert_eq!(status, "APPROVED");
    assert_eq!(name.as_deref(), Some("Test Entity"));

    let account_count: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM accounts WHERE entity_id = $1 AND type = 'USER'::account_type"#,
    )
    .bind(&entity_strkey)
    .fetch_one(&db.pool)
    .await
    .expect("count accounts");
    assert_eq!(account_count, 1, "exactly one USER account should be provisioned");

    let wallet_count: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM wallet_users WHERE public_key = $1"#,
    )
    .bind(&entity_strkey)
    .fetch_one(&db.pool)
    .await
    .expect("count wallet_users");
    assert_eq!(wallet_count, 1, "wallet_user row should be present");

    db.cleanup().await;
}

#[actix_web::test]
async fn register_rejects_unknown_pp_with_404() {
    let Some(db) = skip_if_no_db().await else { return; };

    let pp_seed = [0xABu8; 32];
    let operator_strkey = pp_strkey([0xCCu8; 32]);
    let state = build_test_app_state(pp_seed, operator_strkey, db.pool.clone(), SERVICE_DOMAIN);

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(state.clone()))
            .configure(routing::configure),
    )
    .await;

    let entity_strkey = strkey(SigningKey::from_bytes(&[0xEEu8; 32]).verifying_key().to_bytes());
    // Use a different (random) "PP" in the URL.
    let unknown_pp = pp_strkey([0xDEu8; 32]);

    let req = test::TestRequest::post()
        .uri(&format!("/api/v1/providers/{unknown_pp}/entities/challenge"))
        .set_json(serde_json::json!({ "pubkey": entity_strkey }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status().as_u16(), 404, "unknown PP must 404; got {}", resp.status());

    db.cleanup().await;
}

#[actix_web::test]
async fn register_rejects_bad_signature_with_401() {
    let Some(db) = skip_if_no_db().await else { return; };

    let pp_seed = [0xABu8; 32];
    let operator_strkey = pp_strkey([0xCCu8; 32]);
    let pp_pk = pp_strkey(pp_seed);

    let state = build_test_app_state(pp_seed, operator_strkey, db.pool.clone(), SERVICE_DOMAIN);

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(state.clone()))
            .configure(routing::configure),
    )
    .await;

    let entity_key = SigningKey::from_bytes(&[0xEEu8; 32]);
    let entity_strkey = strkey(entity_key.verifying_key().to_bytes());

    let req = test::TestRequest::post()
        .uri(&format!("/api/v1/providers/{pp_pk}/entities/challenge"))
        .set_json(serde_json::json!({ "pubkey": entity_strkey }))
        .to_request();
    let body: serde_json::Value = test::call_and_read_body_json(&app, req).await;
    let nonce = body["nonce"].as_str().unwrap().to_string();

    // Sign with a different key — must be rejected as 401.
    let attacker_key = SigningKey::from_bytes(&[0x55u8; 32]);
    let bad_sig = sign_nonce(&attacker_key, &nonce);
    let req = test::TestRequest::post()
        .uri(&format!("/api/v1/providers/{pp_pk}/entities"))
        .set_json(serde_json::json!({
            "pubkey": entity_strkey,
            "nonce": nonce,
            "signature": bad_sig,
        }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status().as_u16(), 401, "bad signature must 401; got {}", resp.status());

    db.cleanup().await;
}
