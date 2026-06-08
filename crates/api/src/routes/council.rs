//! Council discover / join / membership.
//!
//! discover: HTTP GET council-platform `/api/v1/public/council?councilId=...`, return the
//!           payload to the operator.
//! join:     POST signed envelope to council-platform `/api/v1/public/provider/join-request`,
//!           insert local `council_memberships` row with status=PENDING.
//! membership GET: read the latest active row for the configured PP.

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
pub struct DiscoverReq {
    pub council_url: String,
}

#[derive(Serialize)]
pub struct DiscoverRes {
    pub message: &'static str,
    pub data: JsonValue,
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
    let base = format!("{}://{}", parsed.scheme(), parsed.host_str().unwrap_or(""));
    let port = parsed
        .port_or_known_default()
        .map(|p| format!(":{p}"))
        .unwrap_or_default();
    let base_url = format!("{base}{port}");

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

    // Council payload should sit under `data` per the reference shape.
    let data = body.get("data").cloned().unwrap_or(body);

    // Cross-check: if a council ID was extracted, it must match the response.
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

    Ok(HttpResponse::Ok().json(DiscoverRes {
        message: "Council discovered",
        data: serde_json::json!({
            "councilUrl": base_url,
            "council": data.get("council").cloned().unwrap_or(JsonValue::Null),
            "jurisdictions": data.get("jurisdictions").cloned().unwrap_or(JsonValue::Null),
            "channels": data.get("channels").cloned().unwrap_or(JsonValue::Null),
            "providers": data.get("providers").cloned().unwrap_or(JsonValue::Null),
        }),
    }))
}

#[derive(Deserialize)]
pub struct JoinReq {
    pub council_url: String,
    pub council_id: Option<String>,
    pub council_name: Option<String>,
    pub council_public_key: Option<String>,
    pub signed_envelope: JsonValue,
}

#[derive(Serialize)]
pub struct JoinRes {
    pub message: &'static str,
    pub membership_id: String,
    pub status: &'static str,
}

#[post("/providers/{pk}/council/join")]
pub async fn post_join(
    state: web::Data<AppState>,
    _auth: OperatorAuth,
    path: web::Path<String>,
    body: web::Json<JoinReq>,
) -> Result<impl Responder, ApiError> {
    let _pk = path.into_inner(); // single-PP shape — ignored; OperatorAuth already gates this
    let req = body.into_inner();

    let parsed = Url::parse(&req.council_url)
        .map_err(|_| ApiError::BadRequest("invalid council_url".into()))?;
    enforce_url_safety(&parsed, &state.config.mode)?;

    let council_id = req
        .council_id
        .clone()
        .or_else(|| extract_council_id(&req.council_url, &parsed))
        .ok_or_else(|| ApiError::BadRequest("council_id required".into()))?;
    let base = format!("{}://{}", parsed.scheme(), parsed.host_str().unwrap_or(""));
    let port = parsed
        .port_or_known_default()
        .map(|p| format!(":{p}"))
        .unwrap_or_default();
    let base_url = format!("{base}{port}");

    // Build the relay payload — spread signedEnvelope + providerUrl.
    let mut payload = req.signed_envelope.clone();
    if let JsonValue::Object(map) = &mut payload {
        map.insert(
            "providerUrl".into(),
            JsonValue::String(state.config.provider_base_url.clone()),
        );
    } else {
        return Err(ApiError::BadRequest("signed_envelope must be a JSON object".into()));
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

    let _ = (req.council_name, join_request_id); // future columns; not in current schema set

    Ok(HttpResponse::Created().json(JoinRes {
        message: "Council join request submitted",
        membership_id: membership.id,
        status: "PENDING",
    }))
}

#[get("/providers/{pk}/council/membership")]
pub async fn get_membership(
    state: web::Data<AppState>,
    _auth: OperatorAuth,
    _path: web::Path<String>,
) -> Result<impl Responder, ApiError> {
    let repo = CouncilMembershipRepo::new(state.pool.clone());
    let memberships = repo.list_active().await?;
    Ok::<_, ApiError>(HttpResponse::Ok().json(serde_json::json!({ "memberships": memberships })))
}

#[post("/providers/{pk}/council/membership")]
pub async fn post_membership(
    state: web::Data<AppState>,
    _auth: OperatorAuth,
    _path: web::Path<String>,
) -> Result<impl Responder, ApiError> {
    // Resync: re-fetch every active membership's council and update status.
    let repo = CouncilMembershipRepo::new(state.pool.clone());
    let memberships = repo.list_active().await?;

    let client = reqwest::Client::builder()
        .timeout(COUNCIL_HTTP_TIMEOUT)
        .build()
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let mut updated = 0usize;
    for m in &memberships {
        let url = format!(
            "{}/api/v1/public/council?councilId={}",
            m.council_url.trim_end_matches('/'),
            urlencoding(&m.channel_auth_id)
        );
        let Ok(resp) = client.get(&url).send().await else {
            continue;
        };
        if !resp.status().is_success() {
            continue;
        }
        let Ok(body) = resp.json::<JsonValue>().await else {
            continue;
        };
        let status_str = body
            .get("data")
            .and_then(|d| d.get("council"))
            .and_then(|c| c.get("status"))
            .and_then(|s| s.as_str())
            .unwrap_or("PENDING");
        let new_status = match status_str {
            "ACTIVE" => CouncilMembershipStatus::Active,
            "REJECTED" => CouncilMembershipStatus::Rejected,
            _ => CouncilMembershipStatus::Pending,
        };
        if new_status != m.status {
            repo.set_status(&m.channel_auth_id, new_status).await?;
            updated += 1;
        }
    }

    Ok(HttpResponse::Ok().json(serde_json::json!({ "memberships": memberships.len(), "updated": updated })))
}

// ---- helpers ----

fn enforce_url_safety(parsed: &Url, mode: &str) -> Result<(), ApiError> {
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(ApiError::BadRequest(
            "council_url must be http(s)".into(),
        ));
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
    if let Some(id) = parsed.query_pairs().find(|(k, _)| k == "council").map(|(_, v)| v.into_owned()) {
        return Some(id);
    }
    // Fragment form: #/join?council=C... (URL strips the fragment).
    let pat = "council=";
    if let Some(start) = raw_url.find(pat) {
        let tail = &raw_url[start + pat.len()..];
        let end = tail.find(|c: char| !c.is_ascii_alphanumeric() && c != '_').unwrap_or(tail.len());
        if end > 0 {
            return Some(tail[..end].to_string());
        }
    }
    None
}

fn urlencoding(s: &str) -> String {
    // Minimal: %-encode anything outside [A-Za-z0-9_-.~]
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
