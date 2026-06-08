//! JWT extractor + operator-pubkey enforcement.
//!
//! For the operator JWT, this also enforces that `claims.sub` matches the configured
//! `OPERATOR_PUBLIC_KEY`. Per pick 9 (PLAN.md) there is no separate allowlist table —
//! the env-bound operator key IS the allowlist.

use crate::error::ApiError;
use crate::state::AppState;
use actix_web::{dev::Payload, web, FromRequest, HttpMessage, HttpRequest};
use futures_util::future::{ready, Ready};
use provider_stack_core::auth::{verify_token, JwtClaims, JwtKind};

pub struct OperatorAuth(pub JwtClaims);

impl FromRequest for OperatorAuth {
    type Error = ApiError;
    type Future = Ready<Result<Self, Self::Error>>;

    fn from_request(req: &HttpRequest, _: &mut Payload) -> Self::Future {
        ready(extract(req, JwtKind::Operator).map(OperatorAuth))
    }
}

pub struct EntityAuth(pub JwtClaims);

impl FromRequest for EntityAuth {
    type Error = ApiError;
    type Future = Ready<Result<Self, Self::Error>>;

    fn from_request(req: &HttpRequest, _: &mut Payload) -> Self::Future {
        ready(extract(req, JwtKind::Entity).map(EntityAuth))
    }
}

fn extract(req: &HttpRequest, expected_kind: JwtKind) -> Result<JwtClaims, ApiError> {
    let token = req
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .ok_or(ApiError::Unauthorized)?;

    let state = req
        .app_data::<web::Data<AppState>>()
        .ok_or_else(|| ApiError::Internal("missing AppState".into()))?;

    let claims = verify_token(state.config.service_auth_secret.as_bytes(), token)
        .map_err(|_| ApiError::Unauthorized)?;

    if claims.kind != expected_kind {
        return Err(ApiError::Forbidden);
    }

    if expected_kind == JwtKind::Operator && claims.sub != state.config.operator_public_key {
        return Err(ApiError::Forbidden);
    }

    req.extensions_mut().insert(claims.clone());
    Ok(claims)
}
