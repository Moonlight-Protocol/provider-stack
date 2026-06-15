//! Operator analytics endpoints. Single-PP: the PP is env-pinned, so the
//! URLs carry no `:pp` segment — the routes are flat under `/provider/`.
//!
//! **Status**: scaffold — each returns an empty-shape JSON wrapped in the
//! `{ data: ... }` envelope the SPA reads, with the field names the SPA
//! consumers expect (metrics → `snapshots`, bundles list → `bundles`,
//! treasury → `address`/`balances`/…). Replace with real implementations.

use crate::envelope::Data;
use crate::error::ApiError;
use crate::middleware_auth::OperatorAuth;
use crate::state::AppState;
use actix_web::{get, web, HttpResponse, Responder};
use chrono::{DateTime, Utc};
use provider_stack_persistence::{BundleStatus, OperationsBundleRepo};
use serde::{Deserialize, Serialize};

macro_rules! stub_get {
    ($fn:ident, $path:literal, $body:expr) => {
        #[get($path)]
        pub async fn $fn(
            _state: web::Data<AppState>,
            _auth: OperatorAuth,
        ) -> Result<impl Responder, ApiError> {
            Ok::<_, ApiError>(HttpResponse::Ok().json(Data::new($body)))
        }
    };
}

stub_get!(get_channels,       "/provider/channels",       serde_json::json!({ "channels": [] }));
stub_get!(get_mempool,        "/provider/mempool",        serde_json::json!({ "slots": [] }));
stub_get!(
    get_treasury,
    "/provider/treasury",
    serde_json::json!({
        "address": "",
        "sequence": "0",
        "balances": [],
        "lastModifiedLedger": 0
    })
);
stub_get!(get_utxos,          "/provider/utxos",          serde_json::json!({ "utxos": [] }));
stub_get!(get_transactions,   "/provider/transactions",   serde_json::json!({ "transactions": [] }));
stub_get!(get_audit_export,   "/provider/audit-export",   serde_json::json!({ "entries": [] }));
stub_get!(
    get_metrics,
    "/provider/metrics",
    serde_json::json!({ "rangeMin": 0, "since": "", "snapshots": [] })
);

#[derive(Deserialize)]
pub struct ListBundlesQuery {
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 {
    100
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentBundleSummary {
    pub id: String,
    pub status: String,
    pub channel_contract_id: Option<String>,
    pub entity_name: Option<String>,
    pub jurisdictions: Vec<String>,
    pub amount: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BundlesListPayload {
    pub bundles: Vec<RecentBundleSummary>,
}

fn bundle_status_to_string(s: BundleStatus) -> String {
    use BundleStatus::*;
    match s {
        Pending => "PENDING",
        Processing => "PROCESSING",
        Completed => "COMPLETED",
        Failed => "FAILED",
        Expired => "EXPIRED",
    }
    .to_string()
}

#[get("/provider/bundles")]
pub async fn get_bundles(
    state: web::Data<AppState>,
    _auth: OperatorAuth,
    query: web::Query<ListBundlesQuery>,
) -> Result<impl Responder, ApiError> {
    let limit = query.limit.clamp(1, 500);
    let repo = OperationsBundleRepo::new(state.pool.clone());
    let rows = repo.list_recent_with_entity(limit).await?;
    let bundles: Vec<RecentBundleSummary> = rows
        .into_iter()
        .map(|r| RecentBundleSummary {
            id: r.id,
            status: bundle_status_to_string(r.status),
            channel_contract_id: r.channel_contract_id,
            entity_name: r.entity_name,
            jurisdictions: r.entity_jurisdictions.unwrap_or_default(),
            amount: None,
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
        .collect();
    Ok(HttpResponse::Ok().json(Data::new(BundlesListPayload { bundles })))
}

#[get("/provider/transactions/{tx_id}")]
pub async fn get_transaction(
    _state: web::Data<AppState>,
    _auth: OperatorAuth,
    _path: web::Path<String>,
) -> Result<impl Responder, ApiError> {
    Err::<HttpResponse, _>(ApiError::NotFound)
}

#[get("/provider/bundles/{bundle_id}")]
pub async fn get_bundle(
    _state: web::Data<AppState>,
    _auth: OperatorAuth,
    _path: web::Path<String>,
) -> Result<impl Responder, ApiError> {
    Err::<HttpResponse, _>(ApiError::NotFound)
}
