//! Council discover / join / membership.
//!
//! Wire shapes match the Deno reference:
//!  - Requests use camelCase keys (`councilUrl`, `councilId`, `signedEnvelope`, …).
//!  - Successful responses are wrapped in `{ data: ... }`.
//!  - GET `/council/membership` returns the latest single membership for the (single) PP
//!    — `{ data: { status, councilUrl, ... } }` — which is what testnet/main.ts polls for
//!    `data.status === "ACTIVE"`.

use crate::envelope::Data;
use crate::error::ApiError;
use crate::middleware_auth::OperatorAuth;
use crate::state::AppState;
use actix_web::{get, post, web, HttpResponse, Responder};
use provider_stack_persistence::{CouncilMembershipRepo, CouncilMembershipStatus};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::time::Duration;
use url::Url;
use uuid::Uuid;

const COUNCIL_HTTP_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_DISCOVER_BODY_BYTES: u64 = 1_048_576;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoverReq {
    pub council_url: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoverPayload {
    pub council_url: String,
    pub council: JsonValue,
    pub jurisdictions: JsonValue,
    pub channels: JsonValue,
    pub providers: JsonValue,
}

#[post("/dashboard/council/discover")]
pub async fn post_discover(
    state: web::Data<AppState>,
    _auth: OperatorAuth,
    body: web::Json<DiscoverReq>,
) -> Result<impl Responder, ApiError> {
    let parsed = Url::parse(&body.council_url)
        .map_err(|_| ApiError::BadRequest("invalid council_url".into()))?;
    enforce_url_safety(&parsed, &state.config.mode)?;

    let council_id = extract_council_id(&body.council_url, &parsed);
    let base_url = base_origin(&parsed);

    let qs = council_id
        .as_deref()
        .map(|id| format!("?councilId={}", urlencoding(id)))
        .unwrap_or_default();
    let url = format!("{base_url}/api/v1/public/council{qs}");

    let client = reqwest::Client::builder()
        .timeout(COUNCIL_HTTP_TIMEOUT)
        .build()
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| ApiError::Internal(format!("council unreachable: {e}")))?;

    let status = response.status();
    if status.as_u16() == 504 {
        return Err(ApiError::Internal("council timeout".into()));
    }
    if !status.is_success() {
        return Err(ApiError::Internal(format!(
            "council returned HTTP {}",
            status.as_u16()
        )));
    }

    if let Some(content_length) = response.content_length() {
        if content_length > MAX_DISCOVER_BODY_BYTES {
            return Err(ApiError::Internal("council response too large".into()));
        }
    }

    let body: JsonValue = response
        .json()
        .await
        .map_err(|e| ApiError::Internal(format!("council body parse: {e}")))?;

    let data = body.get("data").cloned().unwrap_or(body);

    if let Some(expected_id) = &council_id {
        if let Some(council) = data.get("council") {
            if let Some(returned_id) = council.get("channelAuthId").and_then(|v| v.as_str()) {
                if returned_id != expected_id {
                    return Err(ApiError::BadRequest(
                        "council ID in URL does not match the council at this endpoint".into(),
                    ));
                }
            }
        }
    }

    Ok(HttpResponse::Ok().json(Data::new(DiscoverPayload {
        council_url: base_url,
        council: data.get("council").cloned().unwrap_or(JsonValue::Null),
        jurisdictions: data.get("jurisdictions").cloned().unwrap_or(JsonValue::Null),
        channels: data.get("channels").cloned().unwrap_or(JsonValue::Null),
        providers: data.get("providers").cloned().unwrap_or(JsonValue::Null),
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinReq {
    pub council_url: String,
    pub council_id: Option<String>,
    pub council_name: Option<String>,
    pub council_public_key: Option<String>,
    pub label: Option<String>,
    pub contact_email: Option<String>,
    pub signed_envelope: JsonValue,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinPayload {
    pub membership_id: String,
    pub status: &'static str,
}

#[post("/provider/council/join")]
pub async fn post_join(
    state: web::Data<AppState>,
    _auth: OperatorAuth,
    body: web::Json<JoinReq>,
) -> Result<impl Responder, ApiError> {
    let req = body.into_inner();

    let parsed = Url::parse(&req.council_url)
        .map_err(|_| ApiError::BadRequest("invalid council_url".into()))?;
    enforce_url_safety(&parsed, &state.config.mode)?;

    let council_id = req
        .council_id
        .clone()
        .or_else(|| extract_council_id(&req.council_url, &parsed))
        .ok_or_else(|| ApiError::BadRequest("councilId required".into()))?;
    let base_url = base_origin(&parsed);

    let mut payload = req.signed_envelope.clone();
    if let JsonValue::Object(map) = &mut payload {
        map.insert(
            "providerUrl".into(),
            JsonValue::String(state.config.provider_base_url.clone()),
        );
        if let Some(ref label) = req.label {
            map.entry("label").or_insert_with(|| JsonValue::String(label.clone()));
        }
        if let Some(ref ce) = req.contact_email {
            map.entry("contactEmail").or_insert_with(|| JsonValue::String(ce.clone()));
        }
    } else {
        return Err(ApiError::BadRequest("signedEnvelope must be a JSON object".into()));
    }

    let client = reqwest::Client::builder()
        .timeout(COUNCIL_HTTP_TIMEOUT)
        .build()
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let url = format!("{base_url}/api/v1/public/provider/join-request");
    let response = client
        .post(&url)
        .json(&payload)
        .send()
        .await
        .map_err(|e| ApiError::Internal(format!("council unreachable: {e}")))?;

    let status = response.status();
    if status.as_u16() == 409 {
        return Err(ApiError::BadRequest(
            "council reports a pending request already exists".into(),
        ));
    }
    if !status.is_success() {
        return Err(ApiError::Internal(format!(
            "council rejected the request: HTTP {}",
            status.as_u16()
        )));
    }
    let council_body: JsonValue = response
        .json()
        .await
        .map_err(|e| ApiError::Internal(format!("council body parse: {e}")))?;
    let join_request_id = council_body
        .get("data")
        .and_then(|d| d.get("id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let claimed_jurisdictions = req
        .signed_envelope
        .get("payload")
        .and_then(|p| p.get("jurisdictions"))
        .filter(|v| v.is_array())
        .map(|v| v.to_string());

    let repo = CouncilMembershipRepo::new(state.pool.clone());
    let membership_id = Uuid::new_v4().to_string();
    let membership = repo
        .create(
            &membership_id,
            &base_url,
            req.council_public_key.as_deref().unwrap_or(""),
            &council_id,
            claimed_jurisdictions.as_deref(),
        )
        .await?;

    let _ = (req.council_name, join_request_id); // future columns; not in current schema

    Ok(HttpResponse::Created().json(Data::new(JoinPayload {
        membership_id: membership.id,
        status: "PENDING",
    })))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MembershipPayload {
    pub status: String,
    pub channel_auth_id: String,
    pub council_url: String,
    pub council_public_key: String,
    pub claimed_jurisdictions: Option<String>,
}

#[get("/provider/council/membership")]
pub async fn get_membership(
    state: web::Data<AppState>,
    _auth: OperatorAuth,
) -> Result<impl Responder, ApiError> {
    let repo = CouncilMembershipRepo::new(state.pool.clone());
    let memberships = repo.list_active().await?;
    // testnet/main.ts polls until `data.status === "ACTIVE"` — return the most recent
    // membership for this single-PP stack. If none, 404.
    let latest = memberships.first().ok_or(ApiError::NotFound)?;
    let status = match latest.status {
        CouncilMembershipStatus::Active => "ACTIVE",
        CouncilMembershipStatus::Pending => "PENDING",
        CouncilMembershipStatus::Rejected => "REJECTED",
    };
    Ok(HttpResponse::Ok().json(Data::new(MembershipPayload {
        status: status.into(),
        channel_auth_id: latest.channel_auth_id.clone(),
        council_url: latest.council_url.clone(),
        council_public_key: latest.council_public_key.clone(),
        claimed_jurisdictions: latest.claimed_jurisdictions.clone(),
    })))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncPayload {
    pub memberships: usize,
    pub updated: usize,
    /// The newest membership's status post-sync — what the SPA's revocation
    /// check reads. None if there are no memberships.
    pub status: Option<String>,
}

#[post("/provider/council/membership")]
pub async fn post_membership(
    state: web::Data<AppState>,
    _auth: OperatorAuth,
) -> Result<impl Responder, ApiError> {
    let repo = CouncilMembershipRepo::new(state.pool.clone());
    let memberships = repo.list_active().await?;

    // Council-platform answers "is this PP active in this council?" at
    //   GET /api/v1/public/provider/membership-status?councilId=…&publicKey=…
    //   200 → "ACTIVE", 202 → "PENDING", 404 → "NOT_FOUND" (rejected)
    // The PP we ask about is this stack's env-pinned operator key.
    let signing = provider_stack_core::auth::sep10::signing_key_from_seed(
        &state.config.pp_secret_key,
    )?;
    let pp_pubkey = format!(
        "{}",
        stellar_strkey::ed25519::PublicKey(signing.verifying_key().to_bytes())
    );

    let client = reqwest::Client::builder()
        .timeout(COUNCIL_HTTP_TIMEOUT)
        .build()
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let mut updated = 0usize;
    for m in &memberships {
        let url = format!(
            "{}/api/v1/public/provider/membership-status?councilId={}&publicKey={}",
            m.council_url.trim_end_matches('/'),
            urlencoding(&m.channel_auth_id),
            urlencoding(&pp_pubkey),
        );
        let Ok(resp) = client.get(&url).send().await else {
            continue;
        };
        let http_status = resp.status();
        // 404 = council-platform considers the PP not-a-member (either never joined
        // or its request was rejected). Demote local row to REJECTED.
        if http_status.as_u16() == 404 {
            if m.status != CouncilMembershipStatus::Rejected {
                repo.set_status(&m.channel_auth_id, CouncilMembershipStatus::Rejected).await?;
                updated += 1;
            }
            continue;
        }
        // Any other non-2xx: skip — don't clobber the watcher's truth on a transient.
        if !http_status.is_success() && http_status.as_u16() != 202 {
            continue;
        }
        let Ok(body) = resp.json::<JsonValue>().await else {
            continue;
        };
        let Some(status_str) = body.get("status").and_then(|s| s.as_str()) else {
            continue;
        };
        let new_status = match status_str {
            "ACTIVE" => CouncilMembershipStatus::Active,
            "PENDING" => CouncilMembershipStatus::Pending,
            _ => continue,
        };
        if new_status != m.status {
            repo.set_status(&m.channel_auth_id, new_status).await?;
            updated += 1;
        }
    }

    // Re-read after the writes so the response reflects the post-sync truth.
    let latest_status = repo
        .list_active()
        .await?
        .first()
        .map(|m| match m.status {
            CouncilMembershipStatus::Active => "ACTIVE",
            CouncilMembershipStatus::Pending => "PENDING",
            CouncilMembershipStatus::Rejected => "REJECTED",
        })
        .map(str::to_string);

    Ok(HttpResponse::Ok().json(Data::new(SyncPayload {
        memberships: memberships.len(),
        updated,
        status: latest_status,
    })))
}

// ---- helpers ----

fn base_origin(parsed: &Url) -> String {
    let port = parsed
        .port_or_known_default()
        .map(|p| format!(":{p}"))
        .unwrap_or_default();
    format!("{}://{}{}", parsed.scheme(), parsed.host_str().unwrap_or(""), port)
}

fn enforce_url_safety(parsed: &Url, mode: &str) -> Result<(), ApiError> {
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(ApiError::BadRequest("council_url must be http(s)".into()));
    }
    if mode == "development" {
        return Ok(());
    }

    let host = parsed
        .host_str()
        .ok_or_else(|| ApiError::BadRequest("council_url missing host".into()))?
        .to_lowercase();

    if !host.contains('.') {
        return Err(ApiError::BadRequest("rejected internal host".into()));
    }
    if host.ends_with(".local") || host.ends_with(".internal") {
        return Err(ApiError::BadRequest("rejected internal host".into()));
    }
    if host.starts_with("127.")
        || host.starts_with("10.")
        || host.starts_with("192.168.")
        || host == "169.254.169.254"
    {
        return Err(ApiError::BadRequest("rejected internal host".into()));
    }
    Ok(())
}

fn extract_council_id(raw_url: &str, parsed: &Url) -> Option<String> {
    if let Some(id) = parsed
        .query_pairs()
        .find(|(k, _)| k == "council")
        .map(|(_, v)| v.into_owned())
    {
        return Some(id);
    }
    let pat = "council=";
    if let Some(start) = raw_url.find(pat) {
        let tail = &raw_url[start + pat.len()..];
        let end = tail
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .unwrap_or(tail.len());
        if end > 0 {
            return Some(tail[..end].to_string());
        }
    }
    None
}

fn urlencoding(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}
