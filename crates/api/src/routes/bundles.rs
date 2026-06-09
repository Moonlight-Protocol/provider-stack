//! Entity bundle submission + listing.
//!
//! **Status**: POST creates a row via the persistence layer. GET endpoints scaffold.

use crate::error::ApiError;
use crate::middleware_auth::EntityAuth;
use crate::state::AppState;
use actix_web::{get, post, web, HttpResponse, Responder};
use chrono::{Duration, Utc};
use provider_stack_core::bundle::{add_bundle, AddBundleInput};
use provider_stack_persistence::OperationsBundleRepo;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
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
        return Err(ApiError::BadRequest("operations_mlxdr must contain at least one operation".into()));
    }
    if op_count > state.config.bundle_max_operations {
        return Err(ApiError::BadRequest(format!(
            "bundle exceeds BUNDLE_MAX_OPERATIONS ({})",
            state.config.bundle_max_operations
        )));
    }

    // Fee derived from the mempool weight model: cheap_op_weight per op × NETWORK_FEE.
    // This matches the shape mempool/executor use; classification-aware (deposit vs spend) fee
    // calc lands when moonlight-utxo-core is wired in off-chain.
    let fee = (op_count as i64) * (state.config.mempool.cheap_op_weight as i64) * state.config.network_fee;

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
