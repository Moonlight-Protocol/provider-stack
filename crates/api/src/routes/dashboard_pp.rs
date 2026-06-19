//! `/api/v1/dashboard/pp` — the single PP this stack runs.
//!
//! Single-PP: the operator key is env-pinned (`PP_SECRET_KEY` +
//! `OPERATOR_PUBLIC_KEY`); there is no registry, no list, no register call.

use crate::envelope::Data;
use crate::error::ApiError;
use crate::middleware_auth::OperatorAuth;
use crate::state::AppState;
use actix_web::{get, web, HttpResponse, Responder};
use chrono::{DateTime, Utc};
use serde::Serialize;

/// Membership row shape consumed by the Provider Console SPA. `channels` and
/// `councilJurisdictions` need a council-platform follow-up call to populate;
/// the standin returns empty arrays for now. The home/provider views render
/// fine without the asset chips, and the entities section is independent.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PpMembership {
    pub council_url: String,
    pub council_name: Option<String>,
    pub status: String,
    pub channel_auth_id: String,
    pub claimed_jurisdictions: Option<Vec<String>>,
    pub council_jurisdictions: Option<Vec<String>>,
    pub channels: Vec<serde_json::Value>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PpRecord {
    pub public_key: String,
    pub label: Option<String>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub council_memberships: Vec<PpMembership>,
}

fn pp_public_strkey_from_env(state: &AppState) -> Result<String, ApiError> {
    let signing =
        provider_stack_core::auth::sep10::signing_key_from_seed(&state.config.pp_secret_key)?;
    Ok(format!(
        "{}",
        stellar_strkey::ed25519::PublicKey(signing.verifying_key().to_bytes())
    ))
}

fn membership_status_to_string(s: provider_stack_persistence::CouncilMembershipStatus) -> String {
    use provider_stack_persistence::CouncilMembershipStatus::*;
    match s {
        Pending => "PENDING",
        Active => "ACTIVE",
        Rejected => "REJECTED",
    }
    .to_string()
}

async fn load_pp_memberships(state: &AppState) -> Result<Vec<PpMembership>, ApiError> {
    let repo = provider_stack_persistence::CouncilMembershipRepo::new(state.pool.clone());
    let rows = repo.list_active().await?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .ok();

    let mut out = Vec::with_capacity(rows.len());
    for m in rows {
        // Enrich from the council-platform's public summary endpoint:
        //   /api/v1/public/council?councilId=<channelAuthId>
        // Provides the council's display name, the asset/channel chips, and
        // the council's jurisdiction set. Best-effort — if the call fails
        // the row still renders with the local fields.
        let (resolved_name, channels, council_jurs) = match &client {
            Some(c) => {
                let url = format!(
                    "{}/api/v1/public/council?councilId={}",
                    m.council_url.trim_end_matches('/'),
                    urlencoding(&m.channel_auth_id),
                );
                match c.get(&url).send().await {
                    Ok(resp) if resp.status().is_success() => {
                        let body = resp.json::<serde_json::Value>().await.ok();
                        let name = body
                            .as_ref()
                            .and_then(|b| b.get("data"))
                            .and_then(|d| d.get("council"))
                            .and_then(|c| c.get("name"))
                            .and_then(|n| n.as_str())
                            .map(str::to_string);
                        let channels = body
                            .as_ref()
                            .and_then(|b| b.get("data"))
                            .and_then(|d| d.get("channels"))
                            .and_then(|c| c.as_array())
                            .cloned()
                            .unwrap_or_default();
                        let jurs = body
                            .as_ref()
                            .and_then(|b| b.get("data"))
                            .and_then(|d| d.get("jurisdictions"))
                            .and_then(|j| j.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|v| v.get("countryCode"))
                                    .filter_map(|v| v.as_str())
                                    .map(str::to_string)
                                    .collect::<Vec<_>>()
                            });
                        (name, channels, jurs)
                    }
                    _ => (None, Vec::new(), None),
                }
            }
            None => (None, Vec::new(), None),
        };

        out.push(PpMembership {
            council_url: m.council_url,
            council_name: resolved_name.or(m.council_name),
            status: membership_status_to_string(m.status),
            channel_auth_id: m.channel_auth_id,
            claimed_jurisdictions: m
                .claimed_jurisdictions
                .as_deref()
                .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok()),
            council_jurisdictions: council_jurs,
            channels,
        });
    }
    Ok(out)
}

/// Minimal URL-encoder for the councilId query value. Mirrors the one in
/// council.rs without pulling in a heavier dependency.
fn urlencoding(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[get("/dashboard/pp")]
pub async fn get_info(
    state: web::Data<AppState>,
    _auth: OperatorAuth,
) -> Result<impl Responder, ApiError> {
    let public_key = pp_public_strkey_from_env(&state)?;
    let memberships = load_pp_memberships(&state).await?;
    let record = PpRecord {
        public_key,
        label: state.events.current_label(),
        is_active: true,
        created_at: Utc::now(),
        council_memberships: memberships,
    };
    Ok(HttpResponse::Ok().json(Data::new(record)))
}
