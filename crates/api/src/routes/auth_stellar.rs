//! SEP-10 entity auth.
//!
//! `GET  /api/v1/stellar/auth?account=G...` → server-signed challenge envelope (base64 XDR).
//! `POST /api/v1/stellar/auth { transaction }` → entity JWT, after dual-sig + timebound + structure verification.

use crate::error::ApiError;
use crate::state::AppState;
use actix_web::{get, post, web, HttpResponse, Responder};
use provider_stack_core::auth::{
    mint_token,
    sep10::{
        build_challenge, passphrase_for, signing_key_from_seed, verify_signed_envelope,
    },
    JwtKind,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
    state: web::Data<AppState>,
    q: web::Query<ChallengeQuery>,
) -> Result<impl Responder, ApiError> {
    let signing_key = signing_key_from_seed(&state.config.pp_secret_key)?;
    let server_strkey = format!(
        "{}",
        stellar_strkey::ed25519::PublicKey(signing_key.verifying_key().to_bytes())
    );
    let passphrase = passphrase_for(&state.config.network);

    let built = build_challenge(
        &signing_key,
        &server_strkey,
        &q.account,
        passphrase,
        &state.config.service_domain,
        state.config.challenge_ttl.as_secs(),
    )?;

    Ok(HttpResponse::Ok().json(ChallengeRes {
        transaction: built.envelope_xdr_b64,
        network_passphrase: built.network_passphrase,
    }))
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
    state: web::Data<AppState>,
    body: web::Json<VerifyReq>,
) -> Result<impl Responder, ApiError> {
    let signing_key = signing_key_from_seed(&state.config.pp_secret_key)?;
    let server_strkey = format!(
        "{}",
        stellar_strkey::ed25519::PublicKey(signing_key.verifying_key().to_bytes())
    );
    let passphrase = passphrase_for(&state.config.network);

    let verified = verify_signed_envelope(
        &body.transaction,
        &server_strkey,
        passphrase,
        &state.config.service_domain,
    )?;

    let token = mint_token(
        state.config.service_auth_secret.as_bytes(),
        &state.config.service_domain,
        &verified.client_account_strkey,
        JwtKind::Entity,
        &Uuid::new_v4().to_string(),
        state.config.session_ttl.as_secs() as i64,
    )?;

    Ok(HttpResponse::Ok().json(VerifyRes { token }))
}
