//! Entity bundle submission + listing.
//!
//! Wire shapes match the Deno reference: request keys camelCase (`operationsMLXDR`,
//! `channelContractId`), success responses wrapped in `{ data: ... }`. The tests at
//! `local-dev/lib/client/bundle.ts` read `data.data.operationsBundleId`.

use crate::envelope::Data;
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
use soroban_client::{Options, Server};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitReq {
    #[serde(rename = "operationsMLXDR")]
    pub operations_mlxdr: JsonValue,
    pub channel_contract_id: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitPayload {
    pub operations_bundle_id: String,
    pub status: &'static str,
}

#[post("/providers/{pk}/entity/bundles")]
#[tracing::instrument(name = "P_AddOperationsBundle", skip_all)]
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
            "operationsMLXDR must contain at least one operation".into(),
        ));
    }
    if op_count > state.config.bundle_max_operations {
        return Err(ApiError::BadRequest(format!(
            "bundle exceeds BUNDLE_MAX_OPERATIONS ({})",
            state.config.bundle_max_operations
        )));
    }

    tracing::info!(op_count, "bundle submission accepted");
    // Classify by MLXDR type byte.
    let (classified, spend_utxos) = {
        let _span = tracing::info_span!("Bundle.classify").entered();
        tracing::info!("classifying MLXDR slots");
        classify_bundle(&operations_mlxdr)
            .map_err(|e| ApiError::BadRequest(format!("operationsMLXDR: {e}")))?
    };

    // Fetch on-chain balances for Spend UTXOs (if any).
    let spend_balances: Vec<i128> = if !spend_utxos.is_empty() {
        let channel = channel_contract_id
            .as_deref()
            .ok_or_else(|| ApiError::BadRequest(
                "channelContractId is required when bundle contains Spend ops".into(),
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
    let fee_i128 = {
        let _span = tracing::info_span!("Bundle.fee").entered();
        tracing::info!("deriving bundle fee");
        derive_fee_from_classified(&classified, &spend_balances)
    };
    let fee: i64 = fee_i128
        .try_into()
        .map_err(|_| ApiError::Internal("fee overflows i64".into()))?;

    let repo = OperationsBundleRepo::new(state.pool.clone());
    let bundle_id = Uuid::new_v4().to_string();
    let ttl = Utc::now() + Duration::hours(24);

    let _span = tracing::info_span!("Bundle.persist").entered();
    tracing::info!("persisting PENDING bundle row");
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

    Ok(HttpResponse::Created().json(Data::new(SubmitPayload {
        operations_bundle_id: id,
        status: "PENDING",
    })))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityBundlesList {
    pub bundles: Vec<JsonValue>,
}

#[get("/providers/{pk}/entity/bundles")]
pub async fn list_entity(
    _state: web::Data<AppState>,
    _auth: EntityAuth,
    _path: web::Path<String>,
) -> Result<impl Responder, ApiError> {
    Ok::<_, ApiError>(
        HttpResponse::Ok().json(Data::new(EntityBundlesList { bundles: vec![] })),
    )
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleDetail {
    pub id: String,
    pub status: String,
    pub fee: String,
    pub ttl: String,
    #[serde(rename = "operationsMLXDR")]
    pub operations_mlxdr: JsonValue,
    pub created_at: String,
    pub updated_at: Option<String>,
    pub failure_detail: Option<JsonValue>,
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
        Some(b) => {
            let detail = BundleDetail {
                id: b.id,
                status: format!("{:?}", b.status).to_uppercase(),
                fee: b.fee.to_string(),
                ttl: b.ttl.to_rfc3339(),
                operations_mlxdr: b.operations_mlxdr,
                created_at: b.created_at.to_rfc3339(),
                updated_at: Some(b.updated_at.to_rfc3339()),
                failure_detail: b.failure_detail,
            };
            Ok(HttpResponse::Ok().json(Data::new(detail)))
        }
        None => Err(ApiError::NotFound),
    }
}
