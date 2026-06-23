//! Membership convergence-by-query.
//!
//! Ask the council-platform whether this stack's PP is still an active member of
//! each council it holds a membership row for, and reconcile the local
//! `council_memberships.status` to match. Shared by three callers:
//!  - boot, as the can't-miss baseline (alongside channel convergence), so a
//!    `provider_removed` that landed while the standin was down is honoured even
//!    if the in-memory cursor missed it;
//!  - the operator-driven `POST /provider/council/membership` sync;
//!  - the inbound `POST /provider/council/removed` low-trust live signal.
//!
//! Best-effort: a council that is unreachable / 5xx leaves rows untouched, so a
//! transient never clobbers the watcher's truth. Only an authoritative 404
//! ("not a member") demotes a row to REJECTED.

use crate::auth::sep10::signing_key_from_seed;
use crate::config::Config;
use provider_stack_persistence::{CouncilMembershipRepo, CouncilMembershipStatus, PgPool};
use std::time::Duration;

const COUNCIL_HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// Outcome of one convergence pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MembershipConvergence {
    /// Memberships examined.
    pub checked: usize,
    /// Rows whose status changed.
    pub updated: usize,
}

/// Reconcile every membership's status against the council's authoritative
/// `GET /api/v1/public/provider/membership-status` endpoint
/// (200 → ACTIVE, 202 → PENDING, 404 → REJECTED). The PP asked about is this
/// stack's env-pinned operator key.
pub async fn converge_membership_statuses(
    config: &Config,
    pool: &PgPool,
) -> anyhow::Result<MembershipConvergence> {
    let repo = CouncilMembershipRepo::new(pool.clone());
    let memberships = repo.list_active().await?;
    let checked = memberships.len();
    if memberships.is_empty() {
        return Ok(MembershipConvergence::default());
    }

    let signing = signing_key_from_seed(&config.pp_secret_key)?;
    let pp_pubkey = format!(
        "{}",
        stellar_strkey::ed25519::PublicKey(signing.verifying_key().to_bytes())
    );

    let client = reqwest::Client::builder()
        .timeout(COUNCIL_HTTP_TIMEOUT)
        .build()?;

    let mut updated = 0usize;
    for m in &memberships {
        let url = format!(
            "{}/api/v1/public/provider/membership-status?councilId={}&publicKey={}",
            m.council_url.trim_end_matches('/'),
            enc(&m.channel_auth_id),
            enc(&pp_pubkey),
        );
        let Ok(resp) = client.get(&url).send().await else {
            continue;
        };
        let http_status = resp.status();

        // 404 = council considers the PP not-a-member (removed or rejected).
        if http_status.as_u16() == 404 {
            if m.status != CouncilMembershipStatus::Rejected {
                repo.set_status(&m.channel_auth_id, CouncilMembershipStatus::Rejected)
                    .await?;
                updated += 1;
            }
            continue;
        }
        // Any other non-2xx (and not 202): skip — don't clobber on a transient.
        if !http_status.is_success() && http_status.as_u16() != 202 {
            continue;
        }
        let Ok(body) = resp.json::<serde_json::Value>().await else {
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

    Ok(MembershipConvergence { checked, updated })
}

/// Minimal URL query-component encoder — mirrors the api crate's local helper
/// without pulling a dependency for two interpolations.
fn enc(s: &str) -> String {
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
