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
    let repo = OperationsBundleRepo::new(state.pool.clone());
    let bundle_id = Uuid::new_v4().to_string();
    let ttl = Utc::now() + Duration::hours(24);
    let fee = 0; // TODO: classify + fee-calc via moonlight-utxo-core

    let id = add_bundle(
        &repo,
        AddBundleInput {
            bundle_id,
            operations_mlxdr: body.into_inner().operations_mlxdr,
            channel_contract_id: None,
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
