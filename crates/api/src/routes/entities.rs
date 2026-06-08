//! KYC self-register (public, signed nonce). Auto-approves per PR #107.
//!
//! **Status**: scaffold — wires endpoints; SEP-53 signed challenge verify ports next.

use crate::error::ApiError;
use crate::state::AppState;
use actix_web::{post, web, HttpResponse, Responder};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct ChallengeReq {
    pub pubkey: String,
}

#[derive(Serialize)]
pub struct ChallengeRes {
    pub nonce: String,
}

#[post("/providers/{pk}/entities/challenge")]
pub async fn post_challenge(
    state: web::Data<AppState>,
    _path: web::Path<String>,
    body: web::Json<ChallengeReq>,
) -> Result<impl Responder, ApiError> {
    let nonce = state.nonces.issue(&body.pubkey);
    Ok(HttpResponse::Ok().json(ChallengeRes { nonce }))
}

#[derive(Deserialize)]
pub struct RegisterReq {
    pub pubkey: String,
    pub name: Option<String>,
    pub jurisdictions: Option<Vec<String>>,
    pub signed_challenge: String,
}

#[derive(Serialize)]
pub struct RegisterRes {
    pub entity_id: String,
    pub status: String,
}

#[post("/providers/{pk}/entities")]
pub async fn post_register(
    _state: web::Data<AppState>,
    _path: web::Path<String>,
    _body: web::Json<RegisterReq>,
) -> Result<impl Responder, ApiError> {
    Err::<HttpResponse, _>(ApiError::NotImplemented)
}
