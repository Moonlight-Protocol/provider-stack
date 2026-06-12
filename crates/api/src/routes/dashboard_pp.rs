//! `/api/v1/dashboard/pp/register` and `/api/v1/dashboard/pp/list` compat shims.
//!
//! Per PLAN.md the Rust stack is single-PP (one operator per deployment, signing
//! key in `PP_SECRET_KEY` env). These endpoints exist solely for wire-compat with
//! Deno provider-platform's multi-PP shape: register validates that the supplied
//! `secretKey` derives to the env-configured PP and 400s on mismatch; list returns
//! the single configured PP.

use crate::envelope::Data;
use crate::error::ApiError;
use crate::middleware_auth::OperatorAuth;
use crate::state::AppState;
use actix_web::{get, post, web, HttpResponse, Responder};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterReq {
    pub secret_key: String,
    #[serde(default)]
    pub derivation_index: Option<i32>,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PpRecord {
    pub id: String,
    pub public_key: String,
    pub label: Option<String>,
    pub is_active: bool,
    pub derivation_index: i32,
}

fn pp_public_strkey_from_env(state: &AppState) -> Result<String, ApiError> {
    let signing = provider_stack_core::auth::sep10::signing_key_from_seed(
        &state.config.pp_secret_key,
    )?;
    Ok(format!(
        "{}",
        stellar_strkey::ed25519::PublicKey(signing.verifying_key().to_bytes())
    ))
}

fn pp_public_strkey_from_seed(secret_key: &str) -> Result<String, ApiError> {
    let seed = stellar_strkey::ed25519::PrivateKey::from_string(secret_key)
        .map_err(|e| ApiError::BadRequest(format!("secretKey: {e:?}")))?
        .0;
    let signing = SigningKey::from_bytes(&seed);
    Ok(format!(
        "{}",
        stellar_strkey::ed25519::PublicKey(signing.verifying_key().to_bytes())
    ))
}

#[post("/dashboard/pp/register")]
pub async fn post_register(
    state: web::Data<AppState>,
    _auth: OperatorAuth,
    body: web::Json<RegisterReq>,
) -> Result<impl Responder, ApiError> {
    let env_pp = pp_public_strkey_from_env(&state)?;
    let submitted_pp = pp_public_strkey_from_seed(&body.secret_key)?;
    if env_pp != submitted_pp {
        return Err(ApiError::BadRequest(format!(
            "submitted secretKey derives {submitted_pp}, env-configured PP is {env_pp}; \
             this stack only registers its own PP"
        )));
    }
    // Drive the per-PP scope label that the events broadcaster stamps onto every
    // emitted event — the harness asserts `scope.ppLabel == "Testnet E2E PP"`.
    state.events.set_label(body.label.clone());
    let record = PpRecord {
        id: env_pp.clone(),
        public_key: env_pp,
        label: body.label.clone(),
        is_active: true,
        derivation_index: body.derivation_index.unwrap_or(0),
    };
    Ok(HttpResponse::Ok().json(Data::new(record)))
}

#[get("/dashboard/pp/list")]
pub async fn get_list(
    state: web::Data<AppState>,
    _auth: OperatorAuth,
) -> Result<impl Responder, ApiError> {
    let env_pp = pp_public_strkey_from_env(&state)?;
    let record = PpRecord {
        id: env_pp.clone(),
        public_key: env_pp,
        label: None,
        is_active: true,
        derivation_index: 0,
    };
    Ok(HttpResponse::Ok().json(Data::new(vec![record])))
}
