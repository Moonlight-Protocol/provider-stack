//! In-process event broadcast for WebSocket subscribers.
//!
//! Mirrors the Deno provider-platform event taxonomy at
//! `provider-platform/src/core/service/events/event.types.ts`. Each emitted event is a
//! `{ kind, ts, scope: { ppPublicKey, ppLabel }, payload }` envelope wire-compatible with
//! `local-dev/testnet/events-capture` assertions.

use crate::mlxdr::{classify, OperationKind};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use soroban_client::xdr::{
    AccountId, Limits, PublicKey, ReadXdr, ScAddress, ScVal, Uint256,
};
use tokio::sync::broadcast;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventScope {
    #[serde(rename = "ppPublicKey")]
    pub pp_public_key: String,
    #[serde(rename = "ppLabel")]
    pub pp_label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderEvent {
    pub kind: String,
    pub ts: i64,
    pub scope: EventScope,
    pub payload: JsonValue,
}

impl ProviderEvent {
    fn new(kind: &str, scope: EventScope, payload: JsonValue) -> Self {
        Self {
            kind: kind.to_string(),
            ts: Utc::now().timestamp_millis(),
            scope,
            payload,
        }
    }

    pub fn mempool_bundle_added(
        scope: EventScope,
        bundle_id: &str,
        weight: u32,
        channel_contract_id: Option<&str>,
        new_slot: bool,
        entity_name: Option<String>,
        jurisdictions: Vec<String>,
        amount: Option<String>,
    ) -> Self {
        Self::new(
            "mempool.bundle_added",
            scope,
            json!({
                "bundleId": bundle_id,
                "weight": weight,
                "channelContractId": channel_contract_id,
                "newSlot": new_slot,
                "entityName": entity_name,
                "jurisdictions": jurisdictions,
                "amount": amount,
            }),
        )
    }

    pub fn executor_transaction_submitted(
        scope: EventScope,
        tx_hash: &str,
        bundle_ids: &[String],
        channel_contract_id: Option<&str>,
    ) -> Self {
        Self::new(
            "executor.transaction_submitted",
            scope,
            json!({
                "txHash": tx_hash,
                "bundleIds": bundle_ids,
                "channelContractId": channel_contract_id,
            }),
        )
    }

    pub fn verifier_bundle_completed(
        scope: EventScope,
        tx_id: &str,
        bundle_ids: &[String],
        channel_contract_id: Option<&str>,
    ) -> Self {
        Self::new(
            "verifier.bundle_completed",
            scope,
            json!({
                "txId": tx_id,
                "bundleIds": bundle_ids,
                "channelContractId": channel_contract_id,
            }),
        )
    }

    pub fn channel_provider_added(
        scope: EventScope,
        channel_contract_id: &str,
    ) -> Self {
        Self::new(
            "channel.provider_added",
            scope,
            json!({ "channelContractId": channel_contract_id }),
        )
    }

    pub fn bundle_deposit_completed(
        scope: EventScope,
        bundle_id: &str,
        tx_id: &str,
        channel_contract_id: Option<&str>,
        depositor_address: &str,
        amount: &str,
    ) -> Self {
        Self::new(
            "bundle.deposit_completed",
            scope,
            json!({
                "bundleId": bundle_id,
                "txId": tx_id,
                "channelContractId": channel_contract_id,
                "depositorAddress": depositor_address,
                "amount": amount,
            }),
        )
    }

    pub fn bundle_withdraw_completed(
        scope: EventScope,
        bundle_id: &str,
        tx_id: &str,
        channel_contract_id: Option<&str>,
        recipient_address: &str,
        amount: &str,
    ) -> Self {
        Self::new(
            "bundle.withdraw_completed",
            scope,
            json!({
                "bundleId": bundle_id,
                "txId": tx_id,
                "channelContractId": channel_contract_id,
                "recipientAddress": recipient_address,
                "amount": amount,
            }),
        )
    }
}

/// Shared scope state — `pp_public_key` is fixed at boot from `PP_SECRET_KEY`,
/// `pp_label` is updated each time the dashboard `/dashboard/pp/register` compat
/// shim is hit (the harness sets "Testnet E2E PP", browser console SPAs set
/// whatever the operator typed).
#[derive(Clone)]
pub struct EventBroadcaster {
    tx: broadcast::Sender<ProviderEvent>,
    pp_public_key: String,
    pp_label: std::sync::Arc<std::sync::RwLock<Option<String>>>,
}

impl EventBroadcaster {
    pub fn new(capacity: usize, pp_public_key: String) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self {
            tx,
            pp_public_key,
            pp_label: std::sync::Arc::new(std::sync::RwLock::new(None)),
        }
    }

    pub fn send(&self, ev: ProviderEvent) {
        let _ = self.tx.send(ev);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ProviderEvent> {
        self.tx.subscribe()
    }

    pub fn pp_public_key(&self) -> &str {
        &self.pp_public_key
    }

    pub fn current_scope(&self) -> EventScope {
        let label = self
            .pp_label
            .read()
            .ok()
            .and_then(|g| g.clone());
        EventScope {
            pp_public_key: self.pp_public_key.clone(),
            pp_label: label,
        }
    }

    pub fn set_label(&self, label: Option<String>) {
        if let Ok(mut g) = self.pp_label.write() {
            *g = label;
        }
    }

    pub fn current_label(&self) -> Option<String> {
        self.pp_label.read().ok().and_then(|g| g.clone())
    }
}

/// Summary of an operations bundle decoded from its MLXDR slots — drives the
/// payload fields the event-capture harness asserts on. `weight` is `expensive
/// × spend_count + cheap × (create + deposit + withdraw_count)`; `primary_amount`
/// follows the Deno reference: deposit total when there are deposits, else
/// withdraw total when there are withdraws, else the create total (for pure
/// transfers without external value moves).
#[derive(Debug, Clone)]
pub struct BundleSummary {
    pub weight: u32,
    pub primary_amount: Option<String>,
    pub primary_kind: &'static str,
    pub depositor_address: Option<String>,
    pub recipient_address: Option<String>,
}

pub fn summarize_bundle(
    operations_mlxdr: &JsonValue,
    cheap_weight: u32,
    expensive_weight: u32,
) -> anyhow::Result<BundleSummary> {
    let arr = operations_mlxdr
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("operations_mlxdr not an array"))?;
    let mut refs: Vec<&str> = Vec::with_capacity(arr.len());
    for v in arr {
        refs.push(
            v.as_str()
                .ok_or_else(|| anyhow::anyhow!("operations_mlxdr slot not a string"))?,
        );
    }
    let classified = classify(&refs).map_err(|e| anyhow::anyhow!("classify: {e}"))?;

    // Mirror `provider-platform/src/core/service/bundle/bundle.service.ts::calculateBundleWeight`:
    // Spend + Withdraw are EXPENSIVE, Create + Deposit are CHEAP.
    let expensive_count = classified.spend.len() + classified.withdraw.len();
    let cheap_count = classified.create.len() + classified.deposit.len();
    let weight: u32 = expensive_count as u32 * expensive_weight
        + cheap_count as u32 * cheap_weight;

    let total_deposit: i128 = classified.deposit.iter().map(|o| o.amount).sum();
    let total_withdraw: i128 = classified.withdraw.iter().map(|o| o.amount).sum();
    let total_create: i128 = classified.create.iter().map(|o| o.amount).sum();

    let (primary_amount, primary_kind) = if total_deposit > 0 {
        (Some(total_deposit.to_string()), "deposit")
    } else if total_withdraw > 0 {
        (Some(total_withdraw.to_string()), "withdraw")
    } else if total_create > 0 {
        (Some(total_create.to_string()), "send")
    } else {
        (None, "unknown")
    };

    let depositor_address = extract_address_for_kind(&refs, OperationKind::Deposit)?;
    let recipient_address = extract_address_for_kind(&refs, OperationKind::Withdraw)?;

    Ok(BundleSummary {
        weight,
        primary_amount,
        primary_kind,
        depositor_address,
        recipient_address,
    })
}

fn extract_address_for_kind(
    slots: &[&str],
    target: OperationKind,
) -> anyhow::Result<Option<String>> {
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine;
    const ML_PREFIX: [u8; 2] = [0x30, 0xb0];
    for s in slots {
        let bytes = B64.decode(s).map_err(|e| anyhow::anyhow!("base64: {e}"))?;
        if bytes.len() < 3 || bytes[0..2] != ML_PREFIX {
            continue;
        }
        let kind = OperationKind::from_type_byte(bytes[2])
            .map_err(|e| anyhow::anyhow!("type byte: {e}"))?;
        if kind != target {
            continue;
        }
        let outer = ScVal::from_xdr(&bytes[3..], Limits::none())
            .map_err(|e| anyhow::anyhow!("xdr: {e}"))?;
        let scvec = match outer {
            ScVal::Vec(Some(v)) => v.0,
            _ => continue,
        };
        if scvec.is_empty() {
            continue;
        }
        let payload = match &scvec[0] {
            ScVal::Vec(Some(v)) => v.0.clone(),
            _ => continue,
        };
        if payload.is_empty() {
            continue;
        }
        if let ScVal::Address(ScAddress::Account(AccountId(PublicKey::PublicKeyTypeEd25519(
            Uint256(pk),
        )))) = &payload[0]
        {
            let strkey = format!("{}", stellar_strkey::ed25519::PublicKey(*pk));
            return Ok(Some(strkey));
        }
    }
    Ok(None)
}
