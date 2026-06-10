//! HTTP tests for the dashboard PP register / list compat shims.
//!
//! Single-PP semantics internally; multi-PP API shape externally. Register validates that
//! the submitted secretKey derives to the env-configured PP, rejects with 400 otherwise.
//! List returns the single configured PP wrapped in `{ data: [{ ... }] }` (events-capture
//! polls this until the operator's PP appears).

mod common;

use actix_web::{test, web, App};
use common::{build_test_app_state, pp_strkey, TestDb};
use provider_stack_api::routing;
use provider_stack_core::auth::{mint_token, JwtKind};
use serde_json::{json, Value};

const SERVICE_DOMAIN: &str = "smoke.local";

async fn skip_if_no_db() -> Option<TestDb> {
    if std::env::var("DATABASE_URL").is_err() {
        eprintln!("DATABASE_URL not set — skipping dashboard_pp integration test");
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

fn pp_secret_strkey(seed: [u8; 32]) -> String {
    format!("{}", stellar_strkey::ed25519::PrivateKey(seed))
}

#[actix_web::test]
async fn register_accepts_matching_secret_key() {
    let Some(db) = skip_if_no_db().await else { return; };

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
        .uri("/api/v1/dashboard/pp/register")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(json!({
            "secretKey": pp_secret_strkey(pp_seed),
            "derivationIndex": 0,
            "label": "Testnet E2E PP",
        }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status().as_u16(), 200, "expected 200, got {}", resp.status());

    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["data"]["publicKey"], pp_strkey(pp_seed));
    assert_eq!(body["data"]["isActive"], true);
    assert_eq!(body["data"]["label"], "Testnet E2E PP");

    db.cleanup().await;
}

#[actix_web::test]
async fn register_rejects_mismatched_secret_key_with_400() {
    let Some(db) = skip_if_no_db().await else { return; };

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
        .uri("/api/v1/dashboard/pp/register")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(json!({
            "secretKey": pp_secret_strkey([0xDEu8; 32]),
            "derivationIndex": 0,
        }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status().as_u16(), 400, "expected 400 for mismatched key, got {}", resp.status());

    db.cleanup().await;
}

#[actix_web::test]
async fn list_returns_single_configured_pp() {
    let Some(db) = skip_if_no_db().await else { return; };

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

    let req = test::TestRequest::get()
        .uri("/api/v1/dashboard/pp/list")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status().as_u16(), 200, "expected 200, got {}", resp.status());

    let body: Value = test::read_body_json(resp).await;
    let list = body["data"].as_array().expect("data array");
    assert_eq!(list.len(), 1, "expected exactly one PP in list");
    assert_eq!(list[0]["publicKey"], pp_strkey(pp_seed));
    assert_eq!(list[0]["isActive"], true);

    db.cleanup().await;
}

#[actix_web::test]
async fn list_requires_operator_jwt() {
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

    let req = test::TestRequest::get()
        .uri("/api/v1/dashboard/pp/list")
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status().as_u16(), 401, "expected 401 without JWT, got {}", resp.status());

    db.cleanup().await;
}
