//! Provider-held UTXO endpoints for the send-via-email surface (#/pay-name).
//!
//! Keys are derived on the spot from the PP secret + the email
//! (`provider_stack_core::holding`) — no storage; unused/held state comes from
//! chain-scanning balances in index order, mirroring the frontend's own sweep
//! (`-1` = never existed, `0` = spent, `>0` = funded). The sequence is
//! append-only ([used]* then [never-used]*) because targets are always handed
//! out first-unused-first, so the scan stops at the first all-`-1` batch.

use crate::envelope::Data;
use crate::error::ApiError;
use crate::middleware_auth::EntityAuth;
use crate::state::AppState;
use actix_web::{get, web, HttpResponse, Responder};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use provider_stack_core::holding::derive_holding_key;
use provider_stack_sdk::channel::fetch_utxo_balances;
use serde::{Deserialize, Serialize};
use soroban_client::{Options, Server};

const SCAN_BATCH: u32 = 20;
/// POC backstop: emails with more holding keys than this stop resolving.
const SCAN_CAP: u32 = 200;
const MAX_TARGETS: u32 = 10;

pub(crate) fn rpc_server(state: &AppState) -> Result<Server, ApiError> {
    Server::new(
        &state.config.stellar_rpc_url,
        Options {
            allow_http: true,
            ..Options::default()
        },
    )
    .map_err(|e| ApiError::Internal(format!("Server::new: {e:?}")))
}

pub(crate) fn pp_pubkey_strkey(state: &AppState) -> Result<String, ApiError> {
    let signing =
        provider_stack_core::auth::sep10::signing_key_from_seed(&state.config.pp_secret_key)?;
    Ok(format!(
        "{}",
        stellar_strkey::ed25519::PublicKey(signing.verifying_key().to_bytes())
    ))
}

/// (index, 65-byte pubkey, on-chain balance) for every derived key up to the
/// first never-used batch.
async fn scan_holding(
    state: &AppState,
    channel: &str,
    email: &str,
) -> Result<Vec<(u32, [u8; 65], i128)>, ApiError> {
    let server = rpc_server(state)?;
    let pp = pp_pubkey_strkey(state)?;
    let mut out: Vec<(u32, [u8; 65], i128)> = Vec::new();
    let mut index = 0u32;
    while index < SCAN_CAP {
        let batch: Vec<[u8; 65]> = (index..index + SCAN_BATCH)
            .map(|i| derive_holding_key(&state.config.pp_secret_key, email, i).map(|k| k.pubkey65))
            .collect::<anyhow::Result<_>>()
            .map_err(|e| ApiError::Internal(format!("holding derivation: {e}")))?;
        let balances = fetch_utxo_balances(
            &server,
            channel,
            &state.config.network,
            &pp,
            batch.iter().map(|pk| pk.to_vec()).collect(),
        )
        .await
        .map_err(|e| ApiError::Internal(format!("utxo_balances fetch: {e}")))?;
        let all_unused = balances.iter().all(|b| *b == -1);
        for (offset, (pk, balance)) in batch.into_iter().zip(balances).enumerate() {
            out.push((index + offset as u32, pk, balance));
        }
        index += SCAN_BATCH;
        if all_unused {
            return Ok(out);
        }
    }
    tracing::warn!(email = %email, cap = SCAN_CAP, "holding scan hit cap without a never-used batch");
    Ok(out)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PayTargetsQuery {
    pub email: String,
    pub channel: String,
    pub count: Option<u32>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PayTargetsPayload {
    /// Base64 65-byte P-256 pubkeys, never used on-chain, index order.
    pub utxos: Vec<String>,
}

/// Never-used holding UTXO pubkeys for `email` — the CREATE targets a payer
/// uses to send to that email. Works for emails nobody has registered yet:
/// funds simply accumulate under the derivation until someone KYCs with it.
#[get("/provider/entity/holding/targets")]
#[tracing::instrument(name = "P_HoldingTargets", skip_all)]
pub async fn get_targets(
    state: web::Data<AppState>,
    _auth: EntityAuth,
    query: web::Query<PayTargetsQuery>,
) -> Result<impl Responder, ApiError> {
    let email = query.email.trim();
    if email.is_empty() {
        return Err(ApiError::BadRequest("email is required".into()));
    }
    let count = query.count.unwrap_or(1).clamp(1, MAX_TARGETS);
    let scanned = scan_holding(&state, &query.channel, email).await?;
    let utxos: Vec<String> = scanned
        .iter()
        .filter(|(_, _, balance)| *balance == -1)
        .take(count as usize)
        .map(|(_, pk, _)| B64.encode(pk))
        .collect();
    if utxos.len() < count as usize {
        return Err(ApiError::Internal(
            "no free holding keys available for this email".into(),
        ));
    }
    Ok(HttpResponse::Ok().json(Data::new(PayTargetsPayload { utxos })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeldQuery {
    pub channel: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeldUtxoRow {
    /// Base64 65-byte P-256 pubkey.
    pub utxo: String,
    /// Stroops, as string (i128 range).
    pub amount: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeldPayload {
    pub utxos: Vec<HeldUtxoRow>,
}

/// Funded UTXOs the provider holds for the authed entity's registered email.
/// Counted into the entity's balance and spendable through the normal bundle
/// path — submit signs held SPENDs transparently. Empty for entities that
/// registered without an email.
#[get("/provider/entity/holding")]
#[tracing::instrument(name = "P_HoldingList", skip_all)]
pub async fn get_held(
    state: web::Data<AppState>,
    auth: EntityAuth,
    query: web::Query<HeldQuery>,
) -> Result<impl Responder, ApiError> {
    let entities = provider_stack_persistence::EntityRepo::new(state.pool.clone());
    let email = entities.find_by_id(&auth.0.sub).await?.and_then(|e| e.name);
    let utxos = match email {
        None => Vec::new(),
        Some(email) => scan_holding(&state, &query.channel, &email)
            .await?
            .into_iter()
            .filter(|(_, _, balance)| *balance > 0)
            .map(|(_, pk, balance)| HeldUtxoRow {
                utxo: B64.encode(pk),
                amount: balance.to_string(),
            })
            .collect(),
    };
    Ok(HttpResponse::Ok().json(Data::new(HeldPayload { utxos })))
}
