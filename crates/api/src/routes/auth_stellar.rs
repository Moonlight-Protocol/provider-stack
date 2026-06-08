//! SEP-10 entity auth.
//!
//! **Status**: scaffold — challenge construction + verification land when stellar-xdr v27
//! envelope builders are wired (placeholder returns 501).

use crate::error::ApiError;
use crate::state::AppState;
use actix_web::{get, post, web, HttpResponse, Responder};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct ChallengeQuery {
    pub account: String,
}

#[derive(Serialize)]
pub struct ChallengeRes {
    pub transaction: String,
    pub network_passphrase: String,
}

#[get("/stellar/auth")]
pub async fn get_challenge(
    _state: web::Data<AppState>,
    _q: web::Query<ChallengeQuery>,
) -> Result<impl Responder, ApiError> {
    Err::<HttpResponse, _>(ApiError::NotImplemented)
}

#[derive(Deserialize)]
pub struct VerifyReq {
    pub transaction: String,
}

#[derive(Serialize)]
pub struct VerifyRes {
    pub token: String,
}

#[post("/stellar/auth")]
pub async fn post_verify(
    _state: web::Data<AppState>,
    _body: web::Json<VerifyReq>,
) -> Result<impl Responder, ApiError> {
    Err::<HttpResponse, _>(ApiError::NotImplemented)
}
