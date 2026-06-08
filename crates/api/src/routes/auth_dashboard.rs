//! SEP-43 dashboard (operator) auth.

use crate::error::ApiError;
use crate::state::AppState;
use actix_web::{post, web, HttpResponse, Responder};
use provider_stack_core::auth::{mint_token, sep43, JwtKind};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Deserialize)]
pub struct ChallengeReq {
    pub public_key: String,
}

#[derive(Serialize)]
pub struct ChallengeRes {
    pub nonce: String,
}

#[post("/dashboard/auth/challenge")]
pub async fn post_challenge(
    state: web::Data<AppState>,
    body: web::Json<ChallengeReq>,
) -> Result<impl Responder, ApiError> {
    // Reject any non-operator pubkey up front — no point issuing a nonce we'll reject on verify.
    if body.public_key != state.config.operator_public_key {
        return Err(ApiError::Forbidden);
    }
    let nonce = state.nonces.issue(&body.public_key);
    Ok(HttpResponse::Ok().json(ChallengeRes { nonce }))
}

#[derive(Deserialize)]
pub struct VerifyReq {
    pub public_key: String,
    pub nonce: String,
    pub signature: String,
}

#[derive(Serialize)]
pub struct VerifyRes {
    pub token: String,
}

#[post("/dashboard/auth/verify")]
pub async fn post_verify(
    state: web::Data<AppState>,
    body: web::Json<VerifyReq>,
) -> Result<impl Responder, ApiError> {
    if body.public_key != state.config.operator_public_key {
        return Err(ApiError::Forbidden);
    }
    if !state.nonces.consume(&body.nonce, &body.public_key) {
        return Err(ApiError::Unauthorized);
    }
    sep43::verify_signature(&body.public_key, &body.nonce, &body.signature)
        .map_err(|_| ApiError::Unauthorized)?;

    let token = mint_token(
        state.config.service_auth_secret.as_bytes(),
        &state.config.service_domain,
        &body.public_key,
        JwtKind::Operator,
        &Uuid::new_v4().to_string(),
        state.config.session_ttl.as_secs() as i64,
    )?;
    Ok(HttpResponse::Ok().json(VerifyRes { token }))
}
