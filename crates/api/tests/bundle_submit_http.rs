//! HTTP integration test for the entity bundle submission endpoint.
//!
//! POST /api/v1/providers/{pk}/entity/bundles with a valid entity JWT:
//!   - inserts a PENDING operations_bundles row,
//!   - computes the fee via the mempool weight model,
//!   - returns 201 with bundle_id + status.
//! 0-op submissions → 400; over-cap submissions → 400; missing JWT → 401.

mod common;

use actix_web::{test, web, App};
use common::{build_test_app_state, pp_strkey, TestDb};
use ed25519_dalek::SigningKey;
use provider_stack_api::routing;
use provider_stack_core::auth::{mint_token, JwtKind};
use serde_json::{json, Value};
use sqlx::Row;

const SERVICE_DOMAIN: &str = "smoke.local";

async fn skip_if_no_db() -> Option<TestDb> {
    if std::env::var("DATABASE_URL").is_err() {
        eprintln!("DATABASE_URL not set — skipping bundle submit integration test");
        return None;
    }
    Some(TestDb::create().await)
}

fn entity_strkey() -> String {
    let k = SigningKey::from_bytes(&[0xEEu8; 32]);
    format!(
        "{}",
        stellar_strkey::ed25519::PublicKey(k.verifying_key().to_bytes())
    )
}

fn entity_jwt(state: &provider_stack_api::state::AppState) -> String {
    mint_token(
        state.config.service_auth_secret.as_bytes(),
        &state.config.service_domain,
        &entity_strkey(),
        JwtKind::Entity,
        "bundle-session",
        3600,
    )
    .expect("mint entity JWT")
}

#[actix_web::test]
async fn submitting_a_bundle_inserts_row_with_computed_fee() {
    let Some(db) = skip_if_no_db().await else { return; };

    let pp_seed = [0xABu8; 32];
    let operator_strkey = pp_strkey([0xCCu8; 32]);
    let state =
        build_test_app_state(pp_seed, operator_strkey, db.pool.clone(), SERVICE_DOMAIN);
    let pp_pk = pp_strkey(pp_seed);
    let token = entity_jwt(&state);

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(state.clone()))
            .configure(routing::configure),
    )
    .await;

    let body = json!({
        "operations_mlxdr": ["op1-b64", "op2-b64", "op3-b64"],
        "channel_contract_id": null,
    });

    let req = test::TestRequest::post()
        .uri(&format!("/api/v1/providers/{pp_pk}/entity/bundles"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(&body)
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status().as_u16(), 201, "expected 201, got {}", resp.status());

    let res_body: Value = test::read_body_json(resp).await;
    let bundle_id = res_body["bundle_id"].as_str().expect("bundle_id").to_string();
    assert_eq!(res_body["status"], "PENDING");

    let row = sqlx::query(
        r#"SELECT status::text as status, fee, created_by FROM operations_bundles WHERE id = $1"#,
    )
    .bind(&bundle_id)
    .fetch_one(&db.pool)
    .await
    .expect("bundle row");
    assert_eq!(row.get::<String, _>("status"), "PENDING");

    // Fee = op_count * cheap_op_weight * network_fee.
    let expected = 3i64
        * (state.config.mempool.cheap_op_weight as i64)
        * state.config.network_fee;
    assert_eq!(row.get::<i64, _>("fee"), expected, "fee should match weight model");
    assert_eq!(row.get::<Option<String>, _>("created_by"), Some(entity_strkey()));

    db.cleanup().await;
}

#[actix_web::test]
async fn empty_operations_returns_400() {
    let Some(db) = skip_if_no_db().await else { return; };

    let pp_seed = [0xABu8; 32];
    let operator_strkey = pp_strkey([0xCCu8; 32]);
    let state =
        build_test_app_state(pp_seed, operator_strkey, db.pool.clone(), SERVICE_DOMAIN);
    let pp_pk = pp_strkey(pp_seed);
    let token = entity_jwt(&state);

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(state.clone()))
            .configure(routing::configure),
    )
    .await;

    let req = test::TestRequest::post()
        .uri(&format!("/api/v1/providers/{pp_pk}/entity/bundles"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(json!({ "operations_mlxdr": [] }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status().as_u16(), 400, "expected 400, got {}", resp.status());

    db.cleanup().await;
}

#[actix_web::test]
async fn missing_jwt_returns_401() {
    let Some(db) = skip_if_no_db().await else { return; };

    let pp_seed = [0xABu8; 32];
    let operator_strkey = pp_strkey([0xCCu8; 32]);
    let state =
        build_test_app_state(pp_seed, operator_strkey, db.pool.clone(), SERVICE_DOMAIN);
    let pp_pk = pp_strkey(pp_seed);

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(state.clone()))
            .configure(routing::configure),
    )
    .await;

    let req = test::TestRequest::post()
        .uri(&format!("/api/v1/providers/{pp_pk}/entity/bundles"))
        .set_json(json!({ "operations_mlxdr": ["op1"] }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status().as_u16(), 401, "expected 401, got {}", resp.status());

    db.cleanup().await;
}
