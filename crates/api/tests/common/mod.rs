//! Test helpers: per-test Postgres database (created + migrated + dropped) and an
//! `AppState` factory ready to mount with `routing::configure`.
//!
//! Requires `DATABASE_URL` pointing at a reachable Postgres with CREATEDB. If unset, callers
//! should `eprintln + skip` — this module is meant for local + CI use, not unit isolation.

// Shared across many test binaries, each of which uses only a subset of these
// helpers — so some are legitimately unused per-binary.
#![allow(dead_code)]

use ed25519_dalek::SigningKey;
use provider_stack_api::state::AppState;
use provider_stack_core::{
    auth::sep43::NonceStore,
    config::{Config, MempoolConfig},
    events::EventBroadcaster,
};
use provider_stack_persistence::{connect, run_migrations, PgPool};
use sqlx::{Connection, Executor, PgConnection};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

pub struct TestDb {
    pub url: String,
    pub pool: PgPool,
    pub name: String,
    pub admin_url: String,
}

impl TestDb {
    pub async fn create() -> Self {
        let admin_url =
            std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for integration tests");
        let name = format!("test_{}", Uuid::new_v4().simple());

        // Connect to the admin DB and CREATE DATABASE.
        let mut admin_conn = PgConnection::connect(&admin_url)
            .await
            .expect("connect admin db");
        // sqlx 0.9 requires the SQL slice to be 'static. CREATE/DROP DATABASE cannot
        // be parameterised, so leak the small format-produced string.
        let create_sql: &'static str =
            Box::leak(format!("CREATE DATABASE \"{name}\"").into_boxed_str());
        admin_conn
            .execute(create_sql)
            .await
            .expect("create test db");

        let url = make_db_url(&admin_url, &name);
        let pool = connect(&url).await.expect("connect test db");
        run_migrations(&pool)
            .await
            .expect("run migrations on test db");

        TestDb {
            url,
            pool,
            name,
            admin_url,
        }
    }

    pub async fn cleanup(self) {
        // Drop the pool first so we can drop the database.
        self.pool.close().await;
        if let Ok(mut conn) = PgConnection::connect(&self.admin_url).await {
            let drop_sql: &'static str = Box::leak(
                format!("DROP DATABASE IF EXISTS \"{}\" WITH (FORCE)", self.name).into_boxed_str(),
            );
            let _ = conn.execute(drop_sql).await;
        }
    }
}

fn make_db_url(admin_url: &str, db_name: &str) -> String {
    // Naive but adequate: replace the path component after the last `/`.
    let (left, _) = admin_url.rsplit_once('/').unwrap_or((admin_url, ""));
    format!("{left}/{db_name}")
}

pub fn build_test_app_state(
    pp_seed: [u8; 32],
    operator_pubkey_strkey: String,
    pool: PgPool,
    service_domain: &str,
) -> AppState {
    let pp_secret = format!("{}", stellar_strkey::ed25519::PrivateKey(pp_seed));

    let config = Arc::new(Config {
        port: 0,
        // "development" matches the Deno reference's SSRF bypass guard so wiremock
        // can answer from 127.0.0.1 without triggering enforce_url_safety.
        mode: "development".into(),
        log_level: "warn".into(),
        database_url: String::new(),
        network: "standalone".into(),
        network_fee: 1_000_000,
        stellar_rpc_url: String::new(),
        transaction_expiration_offset: 1_000,
        event_watcher_interval: Duration::from_millis(30_000),
        service_domain: service_domain.into(),
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
        events: EventBroadcaster::new(256, "GTESTPP".to_string()),
        nonces,
    }
}

pub fn pp_strkey(seed: [u8; 32]) -> String {
    let key = SigningKey::from_bytes(&seed);
    format!(
        "{}",
        stellar_strkey::ed25519::PublicKey(key.verifying_key().to_bytes())
    )
}

pub fn strkey(pk_bytes: [u8; 32]) -> String {
    format!("{}", stellar_strkey::ed25519::PublicKey(pk_bytes))
}
