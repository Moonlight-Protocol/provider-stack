//! HTTP integration test for the entity bundle submission endpoint.
//!
//! POST /api/v1/providers/{pk}/entity/bundles with a valid entity JWT:
//!   - decodes the MLXDR ops + computes the fee per the provider-platform formula,
//!   - inserts a PENDING operations_bundles row,
//!   - returns 201 with bundle_id + status.
//!
//! 0-op submissions → 400; missing JWT → 401; non-MLXDR strings → 400.

mod common;

use actix_web::{test, web, App};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use common::{build_test_app_state, pp_strkey, TestDb};
use ed25519_dalek::SigningKey;
use provider_stack_api::routing;
use provider_stack_core::auth::{mint_token, JwtKind};
use serde_json::{json, Value};
use soroban_client::xdr::{Int128Parts, Limits, ScAddress, ScBytes, ScVal, ScVec, VecM, WriteXdr};
use sqlx::Row;

const SERVICE_DOMAIN: &str = "smoke.local";
const ML_PREFIX: [u8; 2] = [0x30, 0xb0];

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

/// The submit endpoint gates on the submitter entity being APPROVED, so seed it.
async fn seed_approved_entity(pool: &provider_stack_persistence::PgPool) {
    provider_stack_persistence::EntityRepo::new(pool.clone())
        .create(
            &entity_strkey(),
            provider_stack_persistence::EntityStatus::Approved,
            None,
            None,
            None,
        )
        .await
        .expect("seed approved entity");
}

// ---- MLXDR builders matching moonlight-sdk's wire format ----

fn i128_parts(v: i128) -> Int128Parts {
    Int128Parts {
        hi: (v >> 64) as i64,
        lo: ((v as u128) & 0xFFFF_FFFF_FFFF_FFFF) as u64,
    }
}

fn build_mlxdr(type_byte: u8, op_payload: ScVal) -> String {
    let signature = ScVal::Vec(Some(ScVec(VecM::try_from(Vec::<ScVal>::new()).unwrap())));
    let outer = ScVal::Vec(Some(ScVec(
        VecM::try_from(vec![op_payload, signature]).unwrap(),
    )));
    let xdr_bytes = outer.to_xdr(Limits::none()).unwrap();
    let mut buf = Vec::with_capacity(3 + xdr_bytes.len());
    buf.extend_from_slice(&ML_PREFIX);
    buf.push(type_byte);
    buf.extend_from_slice(&xdr_bytes);
    B64.encode(buf)
}

fn create_mlxdr(utxo: [u8; 65], amount: i128) -> String {
    let payload = ScVal::Vec(Some(ScVec(
        VecM::try_from(vec![
            ScVal::Bytes(ScBytes(utxo.to_vec().try_into().unwrap())),
            ScVal::I128(i128_parts(amount)),
        ])
        .unwrap(),
    )));
    build_mlxdr(0x04, payload)
}

fn deposit_mlxdr(amount: i128) -> String {
    let payload = ScVal::Vec(Some(ScVec(
        VecM::try_from(vec![
            ScVal::Address(ScAddress::Account(soroban_client::xdr::AccountId(
                soroban_client::xdr::PublicKey::PublicKeyTypeEd25519(soroban_client::xdr::Uint256(
                    [0xABu8; 32],
                )),
            ))),
            ScVal::I128(i128_parts(amount)),
            ScVal::Vec(Some(ScVec(VecM::try_from(Vec::<ScVal>::new()).unwrap()))),
        ])
        .unwrap(),
    )));
    build_mlxdr(0x06, payload)
}

#[actix_web::test]
async fn bundle_with_deposit_plus_create_computes_correct_fee() {
    let Some(db) = skip_if_no_db().await else {
        return;
    };

    let pp_seed = [0xABu8; 32];
    let operator_strkey = pp_strkey([0xCCu8; 32]);
    let state = build_test_app_state(pp_seed, operator_strkey, db.pool.clone(), SERVICE_DOMAIN);
    let token = entity_jwt(&state);
    seed_approved_entity(&db.pool).await;

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(state.clone()))
            .configure(routing::configure),
    )
    .await;

    // 1 deposit (1000) + 2 creates (300 + 200 = 500) → fee = 1000 - 500 = 500.
    let ops = vec![
        deposit_mlxdr(1000),
        create_mlxdr([0x11u8; 65], 300),
        create_mlxdr([0x22u8; 65], 200),
    ];

    let req = test::TestRequest::post()
        .uri("/api/v1/provider/entity/bundles")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(json!({ "operationsMLXDR": ops }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(
        resp.status().as_u16(),
        201,
        "expected 201, got {}",
        resp.status()
    );

    let res_body: Value = test::read_body_json(resp).await;
    let bundle_id = res_body["data"]["operationsBundleId"]
        .as_str()
        .expect("data.operationsBundleId")
        .to_string();
    assert_eq!(res_body["data"]["status"], "PENDING");

    let row = sqlx::query(
        r#"SELECT status::text as status, fee, created_by FROM operations_bundles WHERE id = $1"#,
    )
    .bind(&bundle_id)
    .fetch_one(&db.pool)
    .await
    .expect("bundle row");
    assert_eq!(row.get::<String, _>("status"), "PENDING");
    assert_eq!(
        row.get::<i64, _>("fee"),
        500,
        "fee should be 1000 - 500 = 500"
    );
    assert_eq!(
        row.get::<Option<String>, _>("created_by"),
        Some(entity_strkey())
    );

    db.cleanup().await;
}

#[actix_web::test]
async fn empty_operations_returns_400() {
    let Some(db) = skip_if_no_db().await else {
        return;
    };

    let pp_seed = [0xABu8; 32];
    let operator_strkey = pp_strkey([0xCCu8; 32]);
    let state = build_test_app_state(pp_seed, operator_strkey, db.pool.clone(), SERVICE_DOMAIN);
    let token = entity_jwt(&state);
    seed_approved_entity(&db.pool).await;

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(state.clone()))
            .configure(routing::configure),
    )
    .await;

    let req = test::TestRequest::post()
        .uri("/api/v1/provider/entity/bundles")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(json!({ "operationsMLXDR": [] }))
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
async fn missing_jwt_returns_401() {
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
        .uri("/api/v1/provider/entity/bundles")
        .set_json(json!({ "operationsMLXDR": [deposit_mlxdr(100)] }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(
        resp.status().as_u16(),
        401,
        "expected 401, got {}",
        resp.status()
    );

    db.cleanup().await;
}

#[actix_web::test]
async fn non_mlxdr_string_returns_400() {
    let Some(db) = skip_if_no_db().await else {
        return;
    };

    let pp_seed = [0xABu8; 32];
    let operator_strkey = pp_strkey([0xCCu8; 32]);
    let state = build_test_app_state(pp_seed, operator_strkey, db.pool.clone(), SERVICE_DOMAIN);
    let token = entity_jwt(&state);
    seed_approved_entity(&db.pool).await;

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(state.clone()))
            .configure(routing::configure),
    )
    .await;

    let req = test::TestRequest::post()
        .uri("/api/v1/provider/entity/bundles")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(json!({ "operationsMLXDR": ["not-real-mlxdr"] }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(
        resp.status().as_u16(),
        400,
        "expected 400 for non-MLXDR input; got {}",
        resp.status()
    );

    db.cleanup().await;
}

/// UC5: once the standin has been removed from its council (a REJECTED
/// membership and no surviving ACTIVE one), new bundle submissions are refused
/// even from an approved entity — users move to a different PP.
#[actix_web::test]
async fn removed_from_council_rejects_new_bundles() {
    let Some(db) = skip_if_no_db().await else {
        return;
    };

    let pp_seed = [0xABu8; 32];
    let operator_strkey = pp_strkey([0xCCu8; 32]);
    let state = build_test_app_state(pp_seed, operator_strkey, db.pool.clone(), SERVICE_DOMAIN);
    let token = entity_jwt(&state);
    seed_approved_entity(&db.pool).await;

    // Seed a membership and mark it REJECTED — the post-removal state the
    // event-watcher / convergence leaves behind.
    let memberships = provider_stack_persistence::CouncilMembershipRepo::new(db.pool.clone());
    memberships
        .create(
            &uuid::Uuid::new_v4().to_string(),
            "https://council.example",
            "GCOUNCIL",
            "CREMOVEDCOUNCIL",
            Some("[\"US\"]"),
        )
        .await
        .expect("seed membership");
    memberships
        .set_status(
            "CREMOVEDCOUNCIL",
            provider_stack_persistence::CouncilMembershipStatus::Rejected,
        )
        .await
        .expect("reject membership");

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(state.clone()))
            .configure(routing::configure),
    )
    .await;

    // A bundle that would be accepted (201) if the PP were still active.
    let ops = vec![deposit_mlxdr(1000), create_mlxdr([0x11u8; 65], 1000)];
    let req = test::TestRequest::post()
        .uri("/api/v1/provider/entity/bundles")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(json!({ "operationsMLXDR": ops }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(
        resp.status().as_u16(),
        403,
        "expected 403 once removed from council; got {}",
        resp.status()
    );

    db.cleanup().await;
}
