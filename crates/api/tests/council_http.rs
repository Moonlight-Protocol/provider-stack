//! HTTP integration tests for the council surface.
//!
//! Uses wiremock to fake the council-platform HTTP API:
//!   GET  /api/v1/public/council              → returned by discover
//!   POST /api/v1/public/provider/join-request → returned by join
//!
//! And a real per-test Postgres (via `common::TestDb`) so the membership row insertion +
//! readback paths are exercised end-to-end.

mod common;

use actix_web::{test, web, App};
use common::{build_test_app_state, pp_strkey, strkey, TestDb};
use ed25519_dalek::SigningKey;
use provider_stack_api::routing;
use provider_stack_core::auth::{mint_token, JwtKind};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SERVICE_DOMAIN: &str = "smoke.local";

async fn skip_if_no_db() -> Option<TestDb> {
    if std::env::var("DATABASE_URL").is_err() {
        eprintln!("DATABASE_URL not set — skipping council HTTP integration test");
        return None;
    }
    Some(TestDb::create().await)
}

fn operator_jwt(state: &provider_stack_api::state::AppState) -> String {
    mint_token(
        state.config.service_auth_secret.as_bytes(),
        &state.config.service_domain,
        &state.config.operator_public_key,
        JwtKind::Operator,
        "test-session",
        3600,
    )
    .expect("mint operator JWT")
}

#[actix_web::test]
async fn discover_relays_council_payload() {
    let Some(db) = skip_if_no_db().await else {
        return;
    };
    let council = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/v1/public/council"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "council": {
                    "name": "Test Council",
                    "channelAuthId": "CABC123",
                    "publicKey": "GCOUNCIL"
                },
                "jurisdictions": ["US"],
                "channels": [],
                "providers": []
            }
        })))
        .mount(&council)
        .await;

    let pp_seed = [0xABu8; 32];
    let operator_strkey = pp_strkey([0xCCu8; 32]);
    let state = build_test_app_state(pp_seed, operator_strkey, db.pool.clone(), SERVICE_DOMAIN);
    let token = operator_jwt(&state);

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(state.clone()))
            .configure(routing::configure),
    )
    .await;

    let req = test::TestRequest::post()
        .uri("/api/v1/dashboard/council/discover")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(json!({ "councilUrl": format!("{}?council=CABC123", council.uri()) }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert!(
        resp.status().is_success(),
        "discover returned {}",
        resp.status()
    );

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["data"]["council"]["channelAuthId"], "CABC123");
    assert_eq!(body["data"]["jurisdictions"][0], "US");

    db.cleanup().await;
}

#[actix_web::test]
async fn discover_rejects_council_id_mismatch() {
    let Some(db) = skip_if_no_db().await else {
        return;
    };
    let council = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/public/council"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "council": { "channelAuthId": "CXXX999" } }
        })))
        .mount(&council)
        .await;

    let pp_seed = [0xABu8; 32];
    let operator_strkey = pp_strkey([0xCCu8; 32]);
    let state = build_test_app_state(pp_seed, operator_strkey, db.pool.clone(), SERVICE_DOMAIN);
    let token = operator_jwt(&state);
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(state.clone()))
            .configure(routing::configure),
    )
    .await;

    let req = test::TestRequest::post()
        .uri("/api/v1/dashboard/council/discover")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(json!({ "councilUrl": format!("{}?council=CABC123", council.uri()) }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(
        resp.status().as_u16(),
        400,
        "expected 400, got {}",
        resp.status()
    );

    db.cleanup().await;
}

#[actix_web::test]
async fn join_relays_envelope_and_creates_pending_membership() {
    let Some(db) = skip_if_no_db().await else {
        return;
    };
    let council = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/v1/public/provider/join-request"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "data": { "id": "join-req-123" }
        })))
        .mount(&council)
        .await;

    let pp_seed = [0xABu8; 32];
    let operator_strkey = pp_strkey([0xCCu8; 32]);
    let pp_pk = pp_strkey(pp_seed);
    let state = build_test_app_state(pp_seed, operator_strkey, db.pool.clone(), SERVICE_DOMAIN);
    let token = operator_jwt(&state);

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(state.clone()))
            .configure(routing::configure),
    )
    .await;

    let req = test::TestRequest::post()
        .uri("/api/v1/provider/council/join")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(json!({
            "councilUrl": format!("{}?council=CABC123", council.uri()),
            "councilId": "CABC123",
            "councilName": "Test Council",
            "councilPublicKey": "GCOUNCIL",
            "signedEnvelope": {
                "publicKey": pp_pk,
                "payload": { "jurisdictions": ["US"] },
                "signature": "sig"
            }
        }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(
        resp.status().as_u16(),
        201,
        "expected 201, got {}",
        resp.status()
    );

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["data"]["status"], "PENDING");

    // Verify a council_memberships row was inserted with status=PENDING.
    let row: (String, String) = sqlx::query_as(
        r#"SELECT channel_auth_id, status::text FROM council_memberships WHERE channel_auth_id = $1"#,
    )
    .bind("CABC123")
    .fetch_one(&db.pool)
    .await
    .expect("council row");
    assert_eq!(row.0, "CABC123");
    assert_eq!(row.1, "PENDING");

    // GET /council/membership returns it.
    let req = test::TestRequest::get()
        .uri("/api/v1/provider/council/membership")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let body: serde_json::Value = test::call_and_read_body_json(&app, req).await;
    // get_membership returns the latest single membership wrapped under `data`.
    assert_eq!(body["data"]["channelAuthId"], "CABC123");
    assert_eq!(body["data"]["status"], "PENDING");

    db.cleanup().await;
}

#[actix_web::test]
async fn join_requires_operator_jwt() {
    let Some(db) = skip_if_no_db().await else {
        return;
    };

    let pp_seed = [0xABu8; 32];
    let operator_strkey = pp_strkey([0xCCu8; 32]);
    let state = build_test_app_state(pp_seed, operator_strkey, db.pool.clone(), SERVICE_DOMAIN);

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(state.clone()))
            .configure(routing::configure),
    )
    .await;

    let req = test::TestRequest::post()
        .uri("/api/v1/provider/council/join")
        .set_json(json!({
            "councilUrl": "http://example.com",
            "signedEnvelope": {}
        }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(
        resp.status().as_u16(),
        401,
        "expected 401 unauthorized, got {}",
        resp.status()
    );

    db.cleanup().await;
}

// Keep the unused `strkey` warning quiet — it'll be used as soon as more council tests grow.
#[allow(dead_code)]
fn _strkey_for_lints() {
    let _ = strkey(
        SigningKey::from_bytes(&[0u8; 32])
            .verifying_key()
            .to_bytes(),
    );
}
