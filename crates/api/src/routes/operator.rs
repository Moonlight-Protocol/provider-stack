//! Operator analytics endpoints (single-PP, dashboard-side).
//!
//! **Status**: scaffold — each returns an empty-shape JSON for SPA wiring tests.

use crate::error::ApiError;
use crate::middleware_auth::OperatorAuth;
use crate::state::AppState;
use actix_web::{get, web, HttpResponse, Responder};

macro_rules! stub_get {
    ($fn:ident, $path:literal, $body:expr) => {
        #[get($path)]
        pub async fn $fn(
            _state: web::Data<AppState>,
            _auth: OperatorAuth,
            _path: web::Path<String>,
        ) -> Result<impl Responder, ApiError> {
            Ok::<_, ApiError>(HttpResponse::Ok().json($body))
        }
    };
}

stub_get!(get_channels,       "/providers/{pk}/channels",       serde_json::json!({ "channels": [] }));
stub_get!(get_mempool,        "/providers/{pk}/mempool",        serde_json::json!({ "slots": [] }));
stub_get!(get_treasury,       "/providers/{pk}/treasury",       serde_json::json!({ "balance": "0" }));
stub_get!(get_utxos,          "/providers/{pk}/utxos",          serde_json::json!({ "utxos": [] }));
stub_get!(get_transactions,   "/providers/{pk}/transactions",   serde_json::json!({ "transactions": [] }));
stub_get!(get_bundles,        "/providers/{pk}/bundles",        serde_json::json!({ "bundles": [] }));
stub_get!(get_audit_export,   "/providers/{pk}/audit-export",   serde_json::json!({ "entries": [] }));
stub_get!(get_metrics,        "/providers/{pk}/metrics",        serde_json::json!({ "samples": [] }));

#[get("/providers/{pk}/transactions/{tx_id}")]
pub async fn get_transaction(
    _state: web::Data<AppState>,
    _auth: OperatorAuth,
    _path: web::Path<(String, String)>,
) -> Result<impl Responder, ApiError> {
    Err::<HttpResponse, _>(ApiError::NotFound)
}

#[get("/providers/{pk}/bundles/{bundle_id}")]
pub async fn get_bundle(
    _state: web::Data<AppState>,
    _auth: OperatorAuth,
    _path: web::Path<(String, String)>,
) -> Result<impl Responder, ApiError> {
    Err::<HttpResponse, _>(ApiError::NotFound)
}
