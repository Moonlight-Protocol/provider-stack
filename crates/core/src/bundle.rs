//! Bundle ingest: decode MLXDR ops, classify, fetch on-chain UTXO balances for Spends,
//! derive fee per the provider-platform formula, persist a PENDING row.

use crate::error::CoreError;
use crate::mlxdr::{classify, Amounts, Classified, MlxdrError};
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

/// Decode the operations_mlxdr JSON array (`Value::Array<String>`) into a Classified bundle.
///
/// Also returns each Spend op's 65-byte UTXO pubkey in the same order, so callers can fetch
/// on-chain balances and feed them back into `derive_fee_from_classified` in the same order.
pub fn classify_bundle(
    operations_mlxdr: &serde_json::Value,
) -> Result<(Classified, Vec<Vec<u8>>), BundleError> {
    let arr = operations_mlxdr
        .as_array()
        .ok_or(BundleError::OperationsNotArray)?;
    let mut refs: Vec<&str> = Vec::with_capacity(arr.len());
    for v in arr {
        refs.push(v.as_str().ok_or(BundleError::OperationsNotStrings)?);
    }
    let classified = classify(&refs).map_err(BundleError::Mlxdr)?;
    let spend_utxos: Vec<Vec<u8>> = classified.spend.iter().map(|o| o.utxo.clone()).collect();
    Ok((classified, spend_utxos))
}

/// Compute the bundle fee per the provider-platform formula. Caller supplies spend balances
/// aligned with the `Classified.spend` ordering when Spend ops are present (empty slice for
/// pure deposit/create bundles).
pub fn derive_fee_from_classified(classified: &Classified, spend_balances: &[i128]) -> i128 {
    let amounts = Amounts::from_classified(classified, spend_balances);
    crate::mlxdr::calculate_fee(amounts)
}

#[derive(Debug, thiserror::Error)]
pub enum BundleError {
    #[error("operations_mlxdr must be a JSON array")]
    OperationsNotArray,

    #[error("operations_mlxdr entries must be strings")]
    OperationsNotStrings,

    #[error("mlxdr decode: {0}")]
    Mlxdr(#[from] MlxdrError),

    #[error("bundle has Spend ops but no channel_contract_id was supplied")]
    SpendWithoutChannel,
}

pub fn classify_status(_input: &AddBundleInput) -> BundleStatus {
    BundleStatus::Pending
}
