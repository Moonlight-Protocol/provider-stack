//! SEP-10 entity auth.
//!
//! GET  /api/v1/stellar/auth?account=G...  → `{ data: { challenge, networkPassphrase } }`
//! POST /api/v1/stellar/auth { transaction } → `{ data: { token } }`

use crate::envelope::Data;
use crate::error::ApiError;
use crate::state::AppState;
use actix_web::{get, post, web, HttpResponse, Responder};
use provider_stack_core::auth::{
    mint_token,
    sep10::{build_challenge, passphrase_for, signing_key_from_seed, verify_signed_envelope},
    JwtKind,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeQuery {
    pub account: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengePayload {
    pub challenge: String,
    pub network_passphrase: String,
}

#[get("/stellar/auth")]
#[tracing::instrument(name = "P_CreateChallenge", skip_all)]
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
    tracing::info!(account = %q.account, "stellar challenge requested");

    let built = {
        let _s = tracing::info_span!("P_CreateChallengeMemory").entered();
        tracing::info!("building SEP-10 challenge envelope");
        build_challenge(
            &signing_key,
            &server_strkey,
            &q.account,
            passphrase,
            &state.config.service_domain,
            state.config.challenge_ttl.as_secs(),
        )?
    };
    tracing::info!(account = %q.account, "stellar challenge issued");

    Ok(HttpResponse::Ok().json(Data::new(ChallengePayload {
        challenge: built.envelope_xdr_b64,
        network_passphrase: built.network_passphrase,
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyReq {
    pub signed_challenge: String,
    pub pp_public_key: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyPayload {
    pub jwt: String,
    pub entity_status: Option<String>,
    pub kyc_submission_url: Option<String>,
}

#[post("/stellar/auth")]
#[tracing::instrument(name = "P_VerifyChallenge", skip_all)]
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

    tracing::info!("stellar challenge verify started");
    let verified = {
        let _s = tracing::info_span!("P_CompareChallenge").entered();
        tracing::info!("verifying signed challenge envelope");
        verify_signed_envelope(
            &body.signed_challenge,
            &server_strkey,
            passphrase,
            &state.config.service_domain,
        )?
    };

    let _jwt_span = tracing::info_span!("P_GenerateChallengeJWT").entered();
    tracing::info!("minting entity JWT");
    let jwt = mint_token(
        state.config.service_auth_secret.as_bytes(),
        &state.config.service_domain,
        &verified.client_account_strkey,
        JwtKind::Entity,
        &Uuid::new_v4().to_string(),
        state.config.session_ttl.as_secs() as i64,
    )?;

    Ok(HttpResponse::Ok().json(Data::new(VerifyPayload {
        jwt,
        entity_status: None,
        kyc_submission_url: None,
    })))
}
