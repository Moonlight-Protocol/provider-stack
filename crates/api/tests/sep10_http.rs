//! HTTP integration test for the SEP-10 entity auth surface.
//!
//! Drives the real actix-web app (mounted via `routing::configure`) end-to-end:
//!   GET  /api/v1/stellar/auth?account=<G...>  → server-signed challenge envelope
//!   POST /api/v1/stellar/auth { transaction } → entity JWT (after client co-signs)
//!
//! No external services — PostgresPool is created via `connect_lazy` (the SEP-10
//! handlers do not touch the DB; the pool's first call would lazy-connect, but we
//! never make one).

use actix_web::{test, web, App};
use ed25519_dalek::SigningKey;
use provider_stack_api::{routing, state::AppState};
use provider_stack_core::{
    auth::{
        sep10::{attach_signature, network_id, passphrase_for, sign_envelope},
        sep43::NonceStore,
        verify_token, JwtKind,
    },
    config::{Config, MempoolConfig},
    events::EventBroadcaster,
};
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use std::time::Duration;
use stellar_xdr::{Limits, ReadXdr, TransactionEnvelope, WriteXdr};

fn strkey(pk_bytes: [u8; 32]) -> String {
    format!("{}", stellar_strkey::ed25519::PublicKey(pk_bytes))
}

fn skey_secret(seed: [u8; 32]) -> String {
    format!("{}", stellar_strkey::ed25519::PrivateKey(seed))
}

fn make_test_state(pp_seed: [u8; 32], operator_pubkey_strkey: String) -> AppState {
    // PgPool that never connects — SEP-10 handlers don't touch the DB.
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_lazy("postgres://test:test@127.0.0.1:65535/never_used")
        .expect("lazy pool");

    let pp_secret = skey_secret(pp_seed);

    let config = Arc::new(Config {
        port: 0,
        mode: "test".into(),
        log_level: "warn".into(),
        database_url: String::new(),
        network: "standalone".into(),
        network_fee: 1_000_000,
        stellar_rpc_url: String::new(),
        transaction_expiration_offset: 1_000,
        event_watcher_interval: Duration::from_millis(30_000),
        service_domain: "smoke.local".into(),
        service_auth_secret: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".into(),
        provider_base_url: "http://localhost:3010".into(),
        operator_public_key: operator_pubkey_strkey,
        pp_secret_key: pp_secret,
        challenge_ttl: Duration::from_secs(900),
        session_ttl: Duration::from_secs(21_600),
        mempool: MempoolConfig {
            slot_capacity: 10,
            expensive_op_weight: 10,
            cheap_op_weight: 1,
            executor_interval: Duration::from_millis(2_000),
            verifier_interval: Duration::from_millis(2_000),
            ttl_check_interval: Duration::from_millis(5_000),
            max_retry_attempts: 3,
            startup_max_bundle_age: Duration::ZERO,
        },
        bundle_max_operations: 200,
        allowed_origins: vec![],
    });

    let nonces = Arc::new(NonceStore::new(config.challenge_ttl));
    AppState {
        config,
        pool,
        events: EventBroadcaster::default(),
        nonces,
    }
}

#[actix_web::test]
async fn sep10_full_roundtrip_issues_entity_jwt() {
    let pp_seed = [0xABu8; 32];
    let pp_signing = SigningKey::from_bytes(&pp_seed);
    let pp_pubkey = pp_signing.verifying_key().to_bytes();

    let operator_seed = [0xCCu8; 32];
    let operator_signing = SigningKey::from_bytes(&operator_seed);
    let operator_pubkey_strkey = strkey(operator_signing.verifying_key().to_bytes());

    let state = make_test_state(pp_seed, operator_pubkey_strkey);
    let secret_for_jwt = state.config.service_auth_secret.as_bytes().to_vec();

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(state.clone()))
            .configure(routing::configure),
    )
    .await;

    // Entity wallet — what the SPA would have in the browser.
    let entity_seed = [0xEEu8; 32];
    let entity_signing = SigningKey::from_bytes(&entity_seed);
    let entity_strkey = strkey(entity_signing.verifying_key().to_bytes());

    // 1. GET the challenge envelope.
    let req = test::TestRequest::get()
        .uri(&format!("/api/v1/stellar/auth?account={entity_strkey}"))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert!(
        resp.status().is_success(),
        "GET /stellar/auth failed: {}",
        resp.status()
    );
    let body: serde_json::Value = test::read_body_json(resp).await;
    let envelope_b64 = body["data"]["challenge"]
        .as_str()
        .expect("data.challenge field")
        .to_string();
    let net_passphrase = body["data"]["networkPassphrase"].as_str().unwrap().to_string();
    assert_eq!(net_passphrase, passphrase_for(&state.config.network));

    // Sanity: envelope already carries the server signature.
    let mut envelope =
        TransactionEnvelope::from_xdr_base64(&envelope_b64, Limits::none()).expect("parse");
    match &envelope {
        TransactionEnvelope::Tx(v1) => assert_eq!(
            v1.signatures.len(),
            1,
            "server signature should be present"
        ),
        _ => panic!("expected V1 envelope"),
    }

    // 2. Client signs the same envelope.
    let net_id = network_id(&net_passphrase);
    let client_sig = sign_envelope(&entity_signing, &envelope, &net_id).expect("client sig");
    attach_signature(&mut envelope, client_sig).expect("attach");
    let signed_b64 = envelope.to_xdr_base64(Limits::none()).expect("re-encode");

    // 3. POST back, expect JWT.
    let req = test::TestRequest::post()
        .uri("/api/v1/stellar/auth")
        .set_json(serde_json::json!({ "transaction": signed_b64 }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert!(
        resp.status().is_success(),
        "POST /stellar/auth failed: {}",
        resp.status()
    );
    let body: serde_json::Value = test::read_body_json(resp).await;
    let token = body["data"]["token"].as_str().expect("data.token field").to_string();

    // 4. Decode + assert claims.
    let claims = verify_token(&secret_for_jwt, &token).expect("verify_token");
    assert_eq!(claims.sub, entity_strkey, "JWT sub should be entity pubkey");
    assert_eq!(claims.kind, JwtKind::Entity, "JWT kind should be Entity");
    assert!(
        claims.exp > claims.iat,
        "JWT exp must be after iat (got iat={} exp={})",
        claims.iat,
        claims.exp
    );

    // Suppress unused warning on pp_pubkey — kept for diagnostics if the assertion fails.
    let _ = pp_pubkey;
}

#[actix_web::test]
async fn sep10_post_rejects_envelope_without_client_signature() {
    let pp_seed = [0xABu8; 32];
    let operator_seed = [0xCCu8; 32];
    let operator_pubkey_strkey =
        strkey(SigningKey::from_bytes(&operator_seed).verifying_key().to_bytes());
    let state = make_test_state(pp_seed, operator_pubkey_strkey);

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(state.clone()))
            .configure(routing::configure),
    )
    .await;

    let entity_seed = [0xEEu8; 32];
    let entity_strkey = strkey(SigningKey::from_bytes(&entity_seed).verifying_key().to_bytes());

    let req = test::TestRequest::get()
        .uri(&format!("/api/v1/stellar/auth?account={entity_strkey}"))
        .to_request();
    let body: serde_json::Value = test::call_and_read_body_json(&app, req).await;
    let envelope_b64 = body["data"]["challenge"].as_str().unwrap().to_string();

    // Don't co-sign; just post it back as the entity, expect 401.
    let req = test::TestRequest::post()
        .uri("/api/v1/stellar/auth")
        .set_json(serde_json::json!({ "transaction": envelope_b64 }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status().as_u16(), 401, "expected 401, got {}", resp.status());
}
