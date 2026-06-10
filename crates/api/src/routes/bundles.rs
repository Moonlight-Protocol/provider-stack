//! Entity bundle submission + listing.
//!
//! POST decodes the MLXDR ops, classifies them, fetches on-chain UTXO balances for any
//! Spend ops via the channel contract, computes the bundle fee per the provider-platform
//! formula, then persists a PENDING row.

use crate::error::ApiError;
use crate::middleware_auth::EntityAuth;
use crate::state::AppState;
use actix_web::{get, post, web, HttpResponse, Responder};
use chrono::{Duration, Utc};
use provider_stack_core::bundle::{add_bundle, classify_bundle, derive_fee_from_classified, AddBundleInput};
use provider_stack_persistence::OperationsBundleRepo;
use provider_stack_sdk::channel::fetch_utxo_balances;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use soroban_client::{keypair::KeypairBehavior, Options, Server};
use uuid::Uuid;

#[derive(Deserialize)]
pub struct SubmitReq {
    pub operations_mlxdr: JsonValue,
    pub channel_contract_id: Option<String>,
}

#[derive(Serialize)]
pub struct SubmitRes {
    pub bundle_id: String,
    pub status: &'static str,
}

#[post("/providers/{pk}/entity/bundles")]
pub async fn post_submit(
    state: web::Data<AppState>,
    auth: EntityAuth,
    _path: web::Path<String>,
    body: web::Json<SubmitReq>,
) -> Result<impl Responder, ApiError> {
    let SubmitReq { operations_mlxdr, channel_contract_id } = body.into_inner();

    let op_count = operations_mlxdr.as_array().map(|a| a.len()).unwrap_or(0);
    if op_count == 0 {
        return Err(ApiError::BadRequest(
            "operations_mlxdr must contain at least one operation".into(),
        ));
    }
    if op_count > state.config.bundle_max_operations {
        return Err(ApiError::BadRequest(format!(
            "bundle exceeds BUNDLE_MAX_OPERATIONS ({})",
            state.config.bundle_max_operations
        )));
    }

    // Classify by MLXDR type byte.
    let (classified, spend_utxos) = classify_bundle(&operations_mlxdr)
        .map_err(|e| ApiError::BadRequest(format!("operations_mlxdr: {e}")))?;

    // Fetch on-chain balances for Spend UTXOs (if any).
    let spend_balances: Vec<i128> = if !spend_utxos.is_empty() {
        let channel = channel_contract_id
            .as_deref()
            .ok_or_else(|| ApiError::BadRequest(
                "channel_contract_id is required when bundle contains Spend ops".into(),
            ))?;
        let server = Server::new(
            &state.config.stellar_rpc_url,
            Options { allow_http: true, ..Options::default() },
        )
        .map_err(|e| ApiError::Internal(format!("Server::new: {e:?}")))?;
        let signing = provider_stack_core::auth::sep10::signing_key_from_seed(
            &state.config.pp_secret_key,
        )?;
        let pp_pubkey_strkey = format!(
            "{}",
            stellar_strkey::ed25519::PublicKey(signing.verifying_key().to_bytes())
        );
        // Use a soroban-client Keypair purely for its strkey roundtrip (Account::new needs the
        // strkey, which we already have). The simulate doesn't actually validate the source.
        let _ = soroban_client::keypair::Keypair::from_secret(&state.config.pp_secret_key);
        fetch_utxo_balances(
            &server,
            channel,
            &state.config.network,
            &pp_pubkey_strkey,
            spend_utxos,
        )
        .await
        .map_err(|e| ApiError::Internal(format!("utxo_balances fetch: {e}")))?
    } else {
        Vec::new()
    };

    // Derive bundle fee per the provider-platform formula.
    let fee_i128 = derive_fee_from_classified(&classified, &spend_balances);
    let fee: i64 = fee_i128
        .try_into()
        .map_err(|_| ApiError::Internal("fee overflows i64".into()))?;

    let repo = OperationsBundleRepo::new(state.pool.clone());
    let bundle_id = Uuid::new_v4().to_string();
    let ttl = Utc::now() + Duration::hours(24);

    let id = add_bundle(
        &repo,
        AddBundleInput {
            bundle_id,
            operations_mlxdr,
            channel_contract_id,
            submitter_account_id: auth.0.sub.clone(),
        },
        fee,
        ttl,
    )
    .await?;

    Ok(HttpResponse::Created().json(SubmitRes {
        bundle_id: id,
        status: "PENDING",
    }))
}

#[get("/providers/{pk}/entity/bundles")]
pub async fn list_entity(
    _state: web::Data<AppState>,
    _auth: EntityAuth,
    _path: web::Path<String>,
) -> Result<impl Responder, ApiError> {
    Ok::<_, ApiError>(HttpResponse::Ok().json(serde_json::json!({ "bundles": [] })))
}

#[get("/providers/{pk}/entity/bundles/{bundle_id}")]
pub async fn get_entity_bundle(
    state: web::Data<AppState>,
    _auth: EntityAuth,
    path: web::Path<(String, String)>,
) -> Result<impl Responder, ApiError> {
    let (_pk, bundle_id) = path.into_inner();
    let repo = OperationsBundleRepo::new(state.pool.clone());
    match repo.find_by_id(&bundle_id).await? {
        Some(b) => Ok(HttpResponse::Ok().json(b)),
        None => Err(ApiError::NotFound),
    }
}
