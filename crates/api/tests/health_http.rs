//! HTTP integration test for `/api/v1/health`.
//!
//! Two paths:
//!   * DB down — a lazy pool pointed at an unreachable port (no live DB needed);
//!     the bounded `SELECT 1` fails fast and the endpoint must return 503 with
//!     `deps.db == "error"`.
//!   * DB ok — a per-test migrated database (skipped with an `eprintln` when
//!     `DATABASE_URL` is unset); the probe succeeds and the endpoint returns
//!     200 with `deps.db == "ok"`.

mod common;

use actix_web::{test, web, App};
use ed25519_dalek::SigningKey;
use provider_stack_api::routing;
use sqlx::postgres::PgPoolOptions;

fn operator_strkey() -> String {
    let operator_seed = [0xCCu8; 32];
    common::strkey(
        SigningKey::from_bytes(&operator_seed)
            .verifying_key()
            .to_bytes(),
    )
}

#[actix_web::test]
async fn health_reports_503_when_db_unreachable() {
    // Lazy pool that never reaches a real server — the first query (our probe)
    // fails fast, exercising the DB-down branch without a live DB.
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(std::time::Duration::from_millis(500))
        .connect_lazy("postgres://test:test@127.0.0.1:65535/never_used")
        .expect("lazy pool");

    let state = common::build_test_app_state([0xABu8; 32], operator_strkey(), pool, "smoke.local");

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(state.clone()))
            .configure(routing::configure),
    )
    .await;

    let req = test::TestRequest::get().uri("/api/v1/health").to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(
        resp.status().as_u16(),
        503,
        "unreachable DB should yield 503, got {}",
        resp.status()
    );

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["status"], "error");
    assert_eq!(body["deps"]["db"], "error");
    assert!(body["version"].is_string(), "version field preserved");
}

#[actix_web::test]
async fn health_reports_200_and_db_ok_with_live_db() {
    if std::env::var("DATABASE_URL").is_err() {
        eprintln!("skipping health_reports_200_and_db_ok_with_live_db: DATABASE_URL unset");
        return;
    }

    let db = common::TestDb::create().await;
    let state = common::build_test_app_state(
        [0xABu8; 32],
        operator_strkey(),
        db.pool.clone(),
        "smoke.local",
    );

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(state.clone()))
            .configure(routing::configure),
    )
    .await;

    let req = test::TestRequest::get().uri("/api/v1/health").to_request();
    let resp = test::call_service(&app, req).await;
    assert!(
        resp.status().is_success(),
        "reachable DB should yield 200, got {}",
        resp.status()
    );

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["status"], "ok");
    assert_eq!(body["deps"]["db"], "ok");
    assert!(body["version"].is_string(), "version field preserved");

    db.cleanup().await;
}
