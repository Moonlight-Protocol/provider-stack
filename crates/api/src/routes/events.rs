//! WebSocket events endpoint.
//!
//! Bearer token arrives via the `Sec-WebSocket-Protocol` header per the entity-client
//! contract. The server **selects** `moonlight.events.v1` (matching the Deno reference's
//! `EVENTS_WS_SUBPROTOCOL`) as the negotiated subprotocol on the upgrade response.
//!
//! Heartbeat: actix-ws does not auto-ping. To keep connections alive across testnet's
//! 30–60 s inter-bundle pauses (Stellar ledger close + Verifier polling), we spawn a
//! heartbeat task that sends a protocol-level `Ping` every `HEARTBEAT_INTERVAL`. The
//! client is expected to auto-respond with a `Pong` (standard browser/tungstenite
//! behaviour). We also respond to inbound `Ping` frames with a matching `Pong`. After
//! `IDLE_TIMEOUT` of no inbound activity, we close the connection.

use crate::error::ApiError;
use crate::state::AppState;
use actix_web::{get, web, HttpRequest, HttpResponse};
use actix_ws::Message;
use futures_util::StreamExt;
use provider_stack_core::auth::{verify_token, JwtKind};
use std::time::{Duration, Instant};
use tokio::time::interval;

pub const EVENTS_WS_SUBPROTOCOL: &str = "moonlight.events.v1";
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

#[get("/providers/{pk}/events/ws")]
pub async fn ws_events(
    state: web::Data<AppState>,
    _path: web::Path<String>,
    req: HttpRequest,
    body: web::Payload,
) -> Result<HttpResponse, ApiError> {
    let protocol_header = req
        .headers()
        .get("Sec-WebSocket-Protocol")
        .and_then(|v| v.to_str().ok())
        .ok_or(ApiError::Unauthorized)?;

    let bearer = protocol_header
        .split(',')
        .map(|s| s.trim())
        .find_map(|s| s.strip_prefix("bearer."))
        .ok_or(ApiError::Unauthorized)?;

    let claims = verify_token(state.config.service_auth_secret.as_bytes(), bearer)
        .map_err(|_| ApiError::Unauthorized)?;

    if claims.kind != JwtKind::Entity {
        return Err(ApiError::Forbidden);
    }

    let (mut response, session, msg_stream) =
        actix_ws::handle(&req, body).map_err(|e| ApiError::Internal(e.to_string()))?;

    // Tell the client we chose `moonlight.events.v1` as the negotiated subprotocol.
    response.headers_mut().insert(
        actix_web::http::header::SEC_WEBSOCKET_PROTOCOL,
        actix_web::http::header::HeaderValue::from_static(EVENTS_WS_SUBPROTOCOL),
    );

    let events = state.events.clone();
    actix_web::rt::spawn(run_session(session, msg_stream, events));

    Ok(response)
}

/// One WS session: subscribe to the event broadcaster, send heartbeat pings, respond to
/// inbound pings + pongs, close on inbound `Close`, application errors, or idle timeout.
async fn run_session(
    mut session: actix_ws::Session,
    mut msg_stream: actix_ws::MessageStream,
    events: provider_stack_core::events::EventBroadcaster,
) {
    let mut rx = events.subscribe();
    let mut last_inbound_seen = Instant::now();
    let mut heartbeat = interval(HEARTBEAT_INTERVAL);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Skip the first immediate tick.
    heartbeat.tick().await;

    let close_reason = loop {
        tokio::select! {
            biased;

            _ = heartbeat.tick() => {
                if session.ping(b"").await.is_err() {
                    break "ping send failed";
                }
                if last_inbound_seen.elapsed() > IDLE_TIMEOUT {
                    break "idle timeout";
                }
            }

            ev = rx.recv() => {
                match ev {
                    Ok(event) => {
                        let Ok(payload) = serde_json::to_string(&event) else { continue; };
                        if session.text(payload).await.is_err() {
                            break "text send failed";
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        // Drop a notice so the subscriber can resync; keep the connection open.
                        let _ = session
                            .text(serde_json::json!({ "type": "lagged" }).to_string())
                            .await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        break "broadcast channel closed";
                    }
                }
            }

            msg = msg_stream.next() => {
                match msg {
                    Some(Ok(Message::Ping(bytes))) => {
                        last_inbound_seen = Instant::now();
                        if session.pong(&bytes).await.is_err() {
                            break "pong send failed";
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {
                        last_inbound_seen = Instant::now();
                    }
                    Some(Ok(Message::Text(_) | Message::Binary(_))) => {
                        last_inbound_seen = Instant::now();
                        // Server doesn't act on inbound application messages today.
                    }
                    Some(Ok(Message::Close(_))) | None => break "client closed",
                    Some(Ok(Message::Continuation(_) | Message::Nop)) => {
                        last_inbound_seen = Instant::now();
                    }
                    Some(Err(_)) => break "stream error",
                }
            }
        }
    };

    let _ = session.close(Some(actix_ws::CloseReason {
        code: actix_ws::CloseCode::Normal,
        description: Some(close_reason.into()),
    })).await;
}
