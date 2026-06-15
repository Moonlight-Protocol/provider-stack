//! Operator analytics endpoints. Single-PP: the PP is env-pinned, so the
//! URLs carry no `:pp` segment — the routes are flat under `/provider/`.
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
        ) -> Result<impl Responder, ApiError> {
            Ok::<_, ApiError>(HttpResponse::Ok().json($body))
        }
    };
}

stub_get!(get_channels,       "/provider/channels",       serde_json::json!({ "channels": [] }));
stub_get!(get_mempool,        "/provider/mempool",        serde_json::json!({ "slots": [] }));
stub_get!(get_treasury,       "/provider/treasury",       serde_json::json!({ "balance": "0" }));
stub_get!(get_utxos,          "/provider/utxos",          serde_json::json!({ "utxos": [] }));
stub_get!(get_transactions,   "/provider/transactions",   serde_json::json!({ "transactions": [] }));
stub_get!(get_bundles,        "/provider/bundles",        serde_json::json!({ "bundles": [] }));
stub_get!(get_audit_export,   "/provider/audit-export",   serde_json::json!({ "entries": [] }));
stub_get!(get_metrics,        "/provider/metrics",        serde_json::json!({ "samples": [] }));

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
