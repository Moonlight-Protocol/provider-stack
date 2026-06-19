//! End-to-end WebSocket integration test.
//!
//! Starts a real actix-web HTTP server on a random port, connects a tokio-tungstenite
//! client with `Sec-WebSocket-Protocol: bearer.<jwt>, moonlight.events.v1`, then:
//!  1. Asserts the negotiated subprotocol is `moonlight.events.v1` (matches the Deno
//!     reference's `EVENTS_WS_SUBPROTOCOL`).
//!  2. Publishes a `BundleCompleted` event via the broadcaster; asserts the client
//!     receives the matching Text frame within 1 s.
//!  3. Sends a client Ping; asserts the server returns a Pong.
//!  4. Closes; server side ends cleanly.
//!
//! No external services — the only state path the WS exercises is the in-process
//! event broadcaster. PgPool is created lazy and never connects.

mod common;

use actix_web::{web, App, HttpServer};
use ed25519_dalek::SigningKey;
use futures_util::{SinkExt, StreamExt};
use provider_stack_api::{routes::events::EVENTS_WS_SUBPROTOCOL, routing, state::AppState};
use provider_stack_core::{
    auth::{mint_token, sep43::NonceStore, JwtKind},
    config::{Config, MempoolConfig},
    events::{EventBroadcaster, ProviderEvent},
};
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::{
    client::IntoClientRequest, handshake::client::generate_key, http::HeaderValue,
    protocol::Message,
};

fn strkey(pk_bytes: [u8; 32]) -> String {
    format!("{}", stellar_strkey::ed25519::PublicKey(pk_bytes))
}

fn make_state(events: EventBroadcaster) -> AppState {
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_lazy("postgres://test:test@127.0.0.1:65535/never_used")
        .expect("lazy pool");

    let operator_pubkey = strkey(
        SigningKey::from_bytes(&[0xCCu8; 32])
            .verifying_key()
            .to_bytes(),
    );

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
        operator_public_key: operator_pubkey,
        pp_secret_key: "SBGNEH4QFYR4X4VEU5XN73R374XFLUB3VNMTNOZ5DVZXAMSDJGQ2MMQN".into(),
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

    AppState {
        config,
        pool,
        events,
        nonces: Arc::new(NonceStore::new(Duration::from_secs(900))),
    }
}

fn entity_jwt(state: &AppState) -> String {
    let entity_pubkey = strkey(
        SigningKey::from_bytes(&[0xEEu8; 32])
            .verifying_key()
            .to_bytes(),
    );
    mint_token(
        state.config.service_auth_secret.as_bytes(),
        &state.config.service_domain,
        &entity_pubkey,
        JwtKind::Entity,
        "ws-session",
        3600,
    )
    .expect("mint entity JWT")
}

/// Boot an HttpServer on a random port. Returns (port, EventBroadcaster, server handle).
async fn boot_server() -> (u16, EventBroadcaster, actix_web::dev::ServerHandle) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind random port");
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();

    let events = EventBroadcaster::new(256, "GTESTPP".to_string());
    let state = make_state(events.clone());

    let server = HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(state.clone()))
            .configure(routing::configure)
    })
    .workers(1)
    .listen(listener)
    .expect("listen")
    .run();

    let handle = server.handle();
    tokio::spawn(server);
    // Give actix a tick to bind the listener.
    tokio::time::sleep(Duration::from_millis(100)).await;

    (port, events, handle)
}

#[tokio::test]
async fn ws_subprotocol_negotiated_event_arrives_and_ping_is_answered() {
    let (port, events, handle) = boot_server().await;
    let state = make_state(events.clone()); // separate state instance, same secret → same JWT verification
    let jwt = entity_jwt(&state);

    let mut req = format!("ws://127.0.0.1:{port}/api/v1/providers/PP/events/ws")
        .into_client_request()
        .expect("client request");
    // No whitespace after the comma — tungstenite's client subprotocol parsing splits on
    // `,` without trimming, so " moonlight.events.v1" (with leading space) wouldn't
    // contains()-match the server's "moonlight.events.v1" response.
    req.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        HeaderValue::from_str(&format!("bearer.{jwt},{EVENTS_WS_SUBPROTOCOL}"))
            .expect("header value"),
    );
    // tungstenite requires a Key per RFC 6455.
    req.headers_mut().insert(
        "Sec-WebSocket-Key",
        HeaderValue::from_str(&generate_key()).unwrap(),
    );

    let (mut ws_stream, response) = tokio_tungstenite::connect_async(req)
        .await
        .expect("ws connect");

    // (1) Negotiated subprotocol must be moonlight.events.v1.
    let chosen = response
        .headers()
        .get("Sec-WebSocket-Protocol")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .expect("server must select a subprotocol");
    assert_eq!(
        chosen, EVENTS_WS_SUBPROTOCOL,
        "server picked the wrong subprotocol"
    );

    // (2) Broadcast an event; client must receive matching Text within 1 s.
    events.send(ProviderEvent::verifier_bundle_completed(
        events.current_scope(),
        "TX-WS-1",
        &["BUNDLE-WS-1".to_string()],
        None,
    ));

    let frame = timeout(Duration::from_secs(1), ws_stream.next())
        .await
        .expect("event arrival timeout")
        .expect("stream ended")
        .expect("ws error");

    match frame {
        Message::Text(text) => {
            let parsed: serde_json::Value = serde_json::from_str(&text).expect("json");
            assert_eq!(parsed["kind"], "verifier.bundle_completed");
            assert_eq!(parsed["payload"]["bundleIds"][0], "BUNDLE-WS-1");
            assert_eq!(parsed["payload"]["txId"], "TX-WS-1");
        }
        other => panic!("expected Text, got {other:?}"),
    }

    // (3) Send a client Ping; server must echo back a Pong with the same payload.
    let ping_payload = b"hb-1".to_vec();
    ws_stream
        .send(Message::Ping(ping_payload.clone()))
        .await
        .unwrap();
    let pong = timeout(Duration::from_secs(1), ws_stream.next())
        .await
        .expect("pong timeout")
        .expect("stream ended")
        .expect("ws error");
    match pong {
        Message::Pong(bytes) => {
            assert_eq!(bytes, ping_payload, "pong should echo the ping payload");
        }
        other => panic!("expected Pong, got {other:?}"),
    }

    // (4) Close cleanly.
    ws_stream.close(None).await.ok();
    handle.stop(true).await;
}

#[tokio::test]
async fn ws_rejects_request_without_bearer_protocol() {
    let (port, _events, handle) = boot_server().await;

    // No Sec-WebSocket-Protocol header → the upgrade itself should not be authorised.
    let req = format!("ws://127.0.0.1:{port}/api/v1/providers/PP/events/ws")
        .into_client_request()
        .expect("client request");

    let result = tokio_tungstenite::connect_async(req).await;
    assert!(
        result.is_err(),
        "WS connect should fail without bearer; got Ok response"
    );

    handle.stop(true).await;
}
