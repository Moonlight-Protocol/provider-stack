//! WebSocket events endpoint. Bearer token arrives via `Sec-WebSocket-Protocol` header
//! per the entity-client contract.

use crate::error::ApiError;
use crate::state::AppState;
use actix_web::{get, web, HttpRequest, HttpResponse};
use provider_stack_core::auth::{verify_token, JwtKind};

#[get("/providers/{pk}/events/ws")]
pub async fn ws_events(
    state: web::Data<AppState>,
    _path: web::Path<String>,
    req: HttpRequest,
    body: web::Payload,
) -> Result<HttpResponse, ApiError> {
    // Pull bearer from Sec-WebSocket-Protocol (format: `bearer.<jwt>`).
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

    let (mut response, mut session, _msg_stream) = actix_ws::handle(&req, body)
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    // Echo back the chosen subprotocol per the WS RFC.
    response.headers_mut().insert(
        actix_web::http::header::SEC_WEBSOCKET_PROTOCOL,
        actix_web::http::header::HeaderValue::from_str(&format!("bearer.{bearer}"))
            .map_err(|e| ApiError::Internal(e.to_string()))?,
    );

    let mut rx = state.events.subscribe();
    actix_web::rt::spawn(async move {
        while let Ok(ev) = rx.recv().await {
            if let Ok(text) = serde_json::to_string(&ev) {
                if session.text(text).await.is_err() {
                    break;
                }
            }
        }
        let _ = session.close(None).await;
    });

    Ok(response)
}
