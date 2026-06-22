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
use provider_stack_core::bundle::classify_bundle;
use provider_stack_persistence::{
    BundleStatus, EntityRepo, MempoolMetricRepo, OperationsBundleRepo,
};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

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

stub_get!(
    get_channels,
    "/provider/channels",
    serde_json::json!({ "channels": [] })
);
stub_get!(
    get_mempool,
    "/provider/mempool",
    serde_json::json!({ "slots": [] })
);
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
stub_get!(
    get_utxos,
    "/provider/utxos",
    serde_json::json!({ "utxos": [] })
);
stub_get!(
    get_transactions,
    "/provider/transactions",
    serde_json::json!({ "transactions": [] })
);
stub_get!(
    get_audit_export,
    "/provider/audit-export",
    serde_json::json!({ "entries": [] })
);

#[derive(Deserialize)]
pub struct MetricsQuery {
    #[serde(default = "default_range_min")]
    pub range_min: i64,
}

fn default_range_min() -> i64 {
    360
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricsSnapshot {
    pub recorded_at: DateTime<Utc>,
    pub platform_version: String,
    pub queue_depth: i32,
    pub slot_count: i32,
    pub bundles_completed: i32,
    pub bundles_expired: i32,
    pub bundles_failed: i32,
    pub avg_processing_ms: Option<f64>,
    pub p95_processing_ms: Option<f64>,
    pub throughput_per_min: Option<f64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricsPayload {
    pub range_min: i64,
    pub since: DateTime<Utc>,
    pub snapshots: Vec<MetricsSnapshot>,
}

#[get("/provider/metrics")]
pub async fn get_metrics(
    state: web::Data<AppState>,
    _auth: OperatorAuth,
    query: web::Query<MetricsQuery>,
) -> Result<impl Responder, ApiError> {
    let range_min = query.range_min.clamp(1, 24 * 60);
    let since = Utc::now() - chrono::Duration::minutes(range_min);
    let repo = MempoolMetricRepo::new(state.pool.clone());
    let rows = repo.list_since(since).await?;
    let snapshots = rows
        .into_iter()
        .map(|m| MetricsSnapshot {
            recorded_at: m.recorded_at,
            platform_version: m.platform_version,
            queue_depth: m.queue_depth,
            slot_count: m.slot_count,
            bundles_completed: m.bundles_completed,
            bundles_expired: m.bundles_expired,
            bundles_failed: m.bundles_failed,
            avg_processing_ms: m.avg_processing_ms,
            p95_processing_ms: m.p95_processing_ms,
            throughput_per_min: m.throughput_per_min,
        })
        .collect();
    Ok(HttpResponse::Ok().json(Data::new(MetricsPayload {
        range_min,
        since,
        snapshots,
    })))
}

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

/// Walk the classified bundle and return (primary_amount, primary_kind).
/// Mirrors `events.rs::summarize_bundle` — deposit wins over withdraw wins
/// over create (send). i128 totals are stringified so JSON can carry them
/// past JS's 2^53 limit.
fn primary_amount_and_kind(operations_mlxdr: &JsonValue) -> (Option<String>, &'static str) {
    let Ok((classified, _)) = classify_bundle(operations_mlxdr) else {
        return (None, "unknown");
    };
    let total_deposit: i128 = classified.deposit.iter().map(|o| o.amount).sum();
    let total_withdraw: i128 = classified.withdraw.iter().map(|o| o.amount).sum();
    let total_create: i128 = classified.create.iter().map(|o| o.amount).sum();
    if total_deposit > 0 {
        (Some(total_deposit.to_string()), "deposit")
    } else if total_withdraw > 0 {
        (Some(total_withdraw.to_string()), "withdraw")
    } else if total_create > 0 {
        (Some(total_create.to_string()), "send")
    } else {
        (None, "unknown")
    }
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

    // Re-decode each bundle's MLXDR to surface the primary amount in the list.
    // The bundle row doesn't denormalize amount — we have to classify on read.
    // At dashboard limits (≤500) this is cheap enough; revisit if it grows.
    let mut bundles = Vec::with_capacity(rows.len());
    for r in rows {
        let amount = if let Some(detail) = repo.find_by_id(&r.id).await? {
            primary_amount_and_kind(&detail.operations_mlxdr).0
        } else {
            None
        };
        bundles.push(RecentBundleSummary {
            id: r.id,
            status: bundle_status_to_string(r.status),
            channel_contract_id: r.channel_contract_id,
            entity_name: r.entity_name,
            jurisdictions: r.entity_jurisdictions.unwrap_or_default(),
            amount,
            created_at: r.created_at,
            updated_at: r.updated_at,
        });
    }
    Ok(HttpResponse::Ok().json(Data::new(BundlesListPayload { bundles })))
}

// -----------------------------------------------------------------------------
// GET /provider/bundles/{bundle_id} — per-row detail. SPA reads
// `operations[]` to compute the Action label (Deposit / Withdraw / Send)
// and `amount` to fill the Amount cell on first paint when the list
// hadn't yet enriched the row.
// -----------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleOp {
    pub kind: &'static str,
    pub amount: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleDetail {
    pub id: String,
    pub status: String,
    pub channel_contract_id: Option<String>,
    pub operations: Vec<BundleOp>,
    pub entity_name: Option<String>,
    pub jurisdictions: Vec<String>,
    pub amount: Option<String>,
}

fn op_kind_str(k: provider_stack_core::mlxdr::OperationKind) -> &'static str {
    use provider_stack_core::mlxdr::OperationKind::*;
    match k {
        Create => "create",
        Spend => "spend",
        Deposit => "deposit",
        Withdraw => "withdraw",
    }
}

#[get("/provider/bundles/{bundle_id}")]
pub async fn get_bundle(
    state: web::Data<AppState>,
    _auth: OperatorAuth,
    path: web::Path<String>,
) -> Result<impl Responder, ApiError> {
    let bundle_id = path.into_inner();
    let bundle_repo = OperationsBundleRepo::new(state.pool.clone());
    let bundle = bundle_repo
        .find_by_id(&bundle_id)
        .await?
        .ok_or(ApiError::NotFound)?;

    let (operations, amount) = match classify_bundle(&bundle.operations_mlxdr) {
        Ok((classified, _)) => {
            let mut ops = Vec::new();
            for o in classified
                .deposit
                .iter()
                .chain(classified.withdraw.iter())
                .chain(classified.create.iter())
                .chain(classified.spend.iter())
            {
                ops.push(BundleOp {
                    kind: op_kind_str(o.kind),
                    amount: if o.amount != 0 {
                        Some(o.amount.to_string())
                    } else {
                        None
                    },
                });
            }
            let amount = primary_amount_and_kind(&bundle.operations_mlxdr).0;
            (ops, amount)
        }
        Err(_) => (Vec::new(), None),
    };

    let (entity_name, jurisdictions) = if let Some(submitter) = bundle.created_by.as_deref() {
        let entity_repo = EntityRepo::new(state.pool.clone());
        if let Some(e) = entity_repo.find_by_id(submitter).await? {
            (e.name, e.jurisdictions.unwrap_or_default())
        } else {
            (None, Vec::new())
        }
    } else {
        (None, Vec::new())
    };

    Ok(HttpResponse::Ok().json(Data::new(BundleDetail {
        id: bundle.id,
        status: bundle_status_to_string(bundle.status),
        channel_contract_id: bundle.channel_contract_id,
        operations,
        entity_name,
        jurisdictions,
        amount,
    })))
}

#[get("/provider/transactions/{tx_id}")]
pub async fn get_transaction(
    _state: web::Data<AppState>,
    _auth: OperatorAuth,
    _path: web::Path<String>,
) -> Result<impl Responder, ApiError> {
    Err::<HttpResponse, _>(ApiError::NotFound)
}
