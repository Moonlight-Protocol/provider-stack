//! Row-shape structs for every table, plus enum bindings to Postgres enum types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sqlx::FromRow;

// ---- enums (mapped to Postgres ENUM types declared in 0000_init.sql) ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "entity_status", rename_all = "UPPERCASE")]
pub enum EntityStatus {
    Unverified,
    Approved,
    Pending,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "account_type", rename_all = "UPPERCASE")]
pub enum AccountType {
    Opex,
    User,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "session_status", rename_all = "UPPERCASE")]
pub enum SessionStatus {
    Active,
    Inactive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "challenge_status", rename_all = "UPPERCASE")]
pub enum ChallengeStatus {
    Verified,
    Unverified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "bundle_status", rename_all = "UPPERCASE")]
pub enum BundleStatus {
    Pending,
    Processing,
    Completed,
    Expired,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "transaction_status", rename_all = "UPPERCASE")]
pub enum TransactionStatus {
    Unverified,
    Verified,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "council_membership_status", rename_all = "UPPERCASE")]
pub enum CouncilMembershipStatus {
    Pending,
    Active,
    Rejected,
}

// ---- table row structs ----

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Entity {
    pub id: String,
    pub status: EntityStatus,
    pub name: Option<String>,
    pub jurisdictions: Option<Vec<String>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Option<String>,
    pub updated_by: Option<String>,
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    #[sqlx(rename = "type")]
    pub account_type: AccountType,
    pub entity_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Option<String>,
    pub updated_by: Option<String>,
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub status: SessionStatus,
    pub jwt_token: Option<String>,
    pub account_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Option<String>,
    pub updated_by: Option<String>,
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Challenge {
    pub id: String,
    pub account_id: String,
    pub status: ChallengeStatus,
    pub ttl: DateTime<Utc>,
    pub tx_hash: String,
    pub tx_xdr: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Option<String>,
    pub updated_by: Option<String>,
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct OperationsBundle {
    pub id: String,
    pub status: BundleStatus,
    pub channel_contract_id: Option<String>,
    pub ttl: DateTime<Utc>,
    pub operations_mlxdr: JsonValue,
    pub fee: i64,
    pub retry_count: i32,
    pub last_failure_reason: Option<String>,
    pub failure_detail: Option<JsonValue>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Option<String>,
    pub updated_by: Option<String>,
    pub deleted_at: Option<DateTime<Utc>>,
}

/// Local view of a council's asset-channel lifecycle decision (UC6).
/// `is_disabled = true` means the council has disabled this privacy-channel
/// on-chain; the standin enforces withdraw-only on bundle submission. State
/// is driven by `channel_state_changed` events on channel-auth, plus
/// convergence-by-query from the council on boot / out-of-retention.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct ChannelStateRow {
    pub channel_contract_id: String,
    pub is_disabled: bool,
    pub last_event_ledger: Option<i64>,
    pub updated_at: DateTime<Utc>,
}

/// Flattened join of a bundle row with its submitter entity's identity
/// fields — drives the operator dashboard Operations table without a
/// follow-up per-row lookup.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct RecentBundleRow {
    pub id: String,
    pub status: BundleStatus,
    pub channel_contract_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub entity_name: Option<String>,
    pub entity_jurisdictions: Option<Vec<String>>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Transaction {
    pub id: String,
    pub status: TransactionStatus,
    pub timeout: DateTime<Utc>,
    pub ledger_sequence: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Option<String>,
    pub updated_by: Option<String>,
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct BundleTransaction {
    pub bundle_id: String,
    pub transaction_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Option<String>,
    pub updated_by: Option<String>,
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Utxo {
    pub id: String,
    pub amount: i64,
    pub account_id: String,
    pub spent_by_account_id: Option<String>,
    pub created_at_bundle_id: Option<String>,
    pub spent_at_bundle_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Option<String>,
    pub updated_by: Option<String>,
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct WalletUser {
    pub public_key: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct CouncilMembership {
    pub id: String,
    pub council_url: String,
    pub council_name: Option<String>,
    pub council_public_key: String,
    pub channel_auth_id: String,
    pub status: CouncilMembershipStatus,
    pub config_json: Option<String>,
    pub claimed_jurisdictions: Option<String>,
    pub join_request_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Option<String>,
    pub updated_by: Option<String>,
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct MempoolMetric {
    pub id: i32,
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

/// Insert payload for a `mempool_metrics` snapshot. Grouped so the repository's
/// insert stays within the arg-count budget. `id` and `recorded_at` are DB-set.
pub struct MempoolMetricSnapshot<'a> {
    pub platform_version: &'a str,
    pub queue_depth: i32,
    pub slot_count: i32,
    pub bundles_completed: i32,
    pub bundles_expired: i32,
    pub bundles_failed: i32,
    pub avg_processing_ms: Option<f64>,
    pub p95_processing_ms: Option<f64>,
    pub throughput_per_min: Option<f64>,
}
