//! Bundle ingest: accept entity-submitted MLXDR operations, classify, persist, enqueue.
//!
//! Uses `moonlight-utxo-core` for the bundle classification / weight / priority primitives
//! (same Rust types the on-chain channel contract uses).

use crate::error::CoreError;
use provider_stack_persistence::{BundleStatus, OperationsBundleRepo};

pub struct AddBundleInput {
    pub bundle_id: String,
    pub operations_mlxdr: serde_json::Value,
    pub channel_contract_id: Option<String>,
    pub submitter_account_id: String,
}

pub async fn add_bundle(
    repo: &OperationsBundleRepo,
    input: AddBundleInput,
    fee: i64,
    ttl: chrono::DateTime<chrono::Utc>,
) -> Result<String, CoreError> {
    let row = repo
        .create(
            &input.bundle_id,
            ttl,
            &input.operations_mlxdr,
            fee,
            input.channel_contract_id.as_deref(),
            Some(&input.submitter_account_id),
        )
        .await?;
    Ok(row.id)
}

pub fn classify_status(input: &AddBundleInput) -> BundleStatus {
    // TODO: reach into moonlight-utxo-core's classifier; for the scaffold we mark
    // everything PENDING for the mempool to pick up.
    let _ = input;
    BundleStatus::Pending
}
