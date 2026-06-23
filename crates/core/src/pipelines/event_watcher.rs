//! Event watcher pipeline.
//!
//! For every non-deleted council membership, poll Soroban `getEvents` on that membership's
//! channel-auth contract. On a `provider_added` event whose payload references this stack's
//! PP, promote the membership status to `ACTIVE`. On `provider_removed` mark it `REJECTED`.
//!
//! The cursor (last-seen soroban paging token) is held **in memory only**, per
//! channel-auth-id, for the lifetime of the process — there is no durable cursor
//! store. On a fresh start (or after a process restart) the watcher syncs all
//! available history from the oldest ledger the RPC still retains and converges
//! channel state by querying the council, so a Postgres wipe fully resets the
//! instance with no surviving state.

use crate::config::Config;
use crate::events::{EventBroadcaster, ProviderEvent};
use provider_stack_persistence::{
    ChannelStateRepo, CouncilMembershipRepo, CouncilMembershipStatus, PgPool,
};
use soroban_client::soroban_rpc::{EventResponse, EventType};
use soroban_client::xdr::{ScSymbol, ScVal};
use soroban_client::{EventFilter, Options, Pagination, Server, Topic};
use std::collections::HashMap;
use std::sync::Arc;
use stellar_strkey::Contract;
use tokio::sync::Mutex;
use tokio::time::{interval, MissedTickBehavior};
use tracing::{debug, info, instrument, warn};

const EVENT_LIMIT: u32 = 100;

#[instrument(skip_all, name = "pipeline.event_watcher")]
pub async fn run(config: Arc<Config>, pool: PgPool, events: EventBroadcaster) {
    let server = match Server::new(
        &config.stellar_rpc_url,
        Options {
            allow_http: true,
            ..Options::default()
        },
    ) {
        Ok(s) => s,
        Err(e) => {
            warn!(error = ?e, "event_watcher: Server::new failed; pipeline will not run");
            return;
        }
    };

    // UC6 convergence: on boot, ask the council for the current channel
    // state for every membership we know about, so a disable that landed
    // while the standin was down doesn't get missed (the live event path
    // would only ever apply deltas from the cursor forward).
    if let Err(e) = converge_channel_states(&pool).await {
        warn!(error = %e, "boot-time channel convergence failed");
    }

    // UC5 convergence: same idea for membership. Ask the council whether this PP
    // is still a member of each council it has a row for, so a `provider_removed`
    // that landed while the standin was down is honoured on boot even if the
    // in-memory cursor missed it. Best-effort, like channel convergence.
    match crate::pipelines::membership_convergence::converge_membership_statuses(&config, &pool)
        .await
    {
        Ok(c) if c.updated > 0 => {
            info!(
                updated = c.updated,
                "boot-time membership convergence applied"
            )
        }
        Ok(_) => {}
        Err(e) => warn!(error = %e, "boot-time membership convergence failed"),
    }

    let mut tick = interval(config.event_watcher_interval);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let processing = Arc::new(Mutex::new(false));

    // In-memory cursor per channel-auth-id, advanced across ticks for the life of
    // the process. Lost on restart by design — a fresh process re-syncs from the
    // oldest available ledger and re-converges from the council.
    let mut cursors: HashMap<String, String> = HashMap::new();

    loop {
        tick.tick().await;
        let mut guard = processing.lock().await;
        if *guard {
            continue;
        }
        *guard = true;
        drop(guard);

        if let Err(e) = run_tick(&server, &pool, &events, &mut cursors).await {
            warn!(error = %e, "event_watcher tick failed");
        }
        debug!("event_watcher tick complete");

        let mut guard = processing.lock().await;
        *guard = false;
    }
}

/// One watcher tick. Exposed for the integration test.
///
/// `cursors` is the in-memory per-channel-auth-id paging cursor, owned by the
/// caller and carried across ticks for the life of the process. It is never
/// persisted: an absent entry means "sync from the oldest available ledger".
pub async fn run_tick(
    server: &Server,
    pool: &PgPool,
    events: &EventBroadcaster,
    cursors: &mut HashMap<String, String>,
) -> anyhow::Result<()> {
    let memberships_repo = CouncilMembershipRepo::new(pool.clone());
    let channel_state_repo = ChannelStateRepo::new(pool.clone());
    let memberships = memberships_repo.list_active().await?;

    for m in memberships {
        let pagination = match cursors.get(&m.channel_auth_id) {
            Some(c) => Pagination::Cursor(c.clone()),
            None => {
                // No in-memory cursor (fresh process / first tick for this
                // council) — sync ALL available history from the oldest ledger
                // the RPC still retains, so nothing emitted while we were down is
                // skipped. Convergence-by-query is the can't-miss baseline.
                let oldest = server.get_health().await?.oldest_ledger;
                Pagination::From(oldest.max(1))
            }
        };
        // Topic filter matches any-kind events with 1+ topics. The contract
        // emits provider_added/removed (2 topics) and channel_state_changed
        // (3 topics: kind + channel + asset); a fixed-arity filter would
        // miss one shape, so use a greedy tail.
        let build_filter = || {
            EventFilter::new(EventType::Contract)
                .contract(&m.channel_auth_id)
                .topic(vec![Topic::Any, Topic::Greedy])
        };
        let mut result = server
            .get_events(pagination, vec![build_filter()], EVENT_LIMIT)
            .await;
        // Self-heal "startLedger must be within the ledger range: X - Y" — happens on a
        // fresh Stellar local where ledger 1 is below the retention min, or after a
        // long idle when the cursor falls out of retention. Parse the new minimum from
        // the error and retry from there. Best-effort: if parse fails, leave the error
        // alone for the next tick.
        if let Err(e) = &result {
            if let Some(min_ledger) = parse_min_ledger_from_error(&format!("{e:?}")) {
                cursors.remove(&m.channel_auth_id);
                // UC6: out-of-retention means we can no longer event-replay any
                // disable that landed in the gap. Re-converge from the council's
                // current truth before resuming the live stream.
                if let Err(ce) = converge_channel_states(pool).await {
                    warn!(error = %ce, "out-of-retention recovery: convergence failed");
                }
                result = server
                    .get_events(
                        Pagination::From(min_ledger),
                        vec![build_filter()],
                        EVENT_LIMIT,
                    )
                    .await;
            }
        }
        let response = match result {
            Ok(r) => r,
            Err(e) => {
                warn!(channel = %m.channel_auth_id, error = ?e, "get_events failed");
                continue;
            }
        };

        for event in &response.events {
            apply_event(
                &memberships_repo,
                &channel_state_repo,
                &m.channel_auth_id,
                event,
                events,
            )
            .await?;
        }
        if let Some(new_cursor) = response.cursor {
            cursors.insert(m.channel_auth_id.clone(), new_cursor);
        }
    }
    Ok(())
}

/// UC6 convergence-by-query. For each non-deleted membership, asks the
/// council-platform `GET /api/v1/public/council?councilId=…` for the
/// authoritative per-channel state and overlays it onto the local
/// `channel_states` table. Best-effort — a council-platform unreachable /
/// 5xx leaves the table untouched, the next live event still applies a
/// delta, and we'll re-converge at the next retention recovery or boot.
async fn converge_channel_states(pool: &PgPool) -> anyhow::Result<()> {
    let memberships = CouncilMembershipRepo::new(pool.clone())
        .list_active()
        .await?;
    if memberships.is_empty() {
        return Ok(());
    }
    let channels = ChannelStateRepo::new(pool.clone());
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .ok();
    let Some(client) = client else {
        return Ok(());
    };
    for m in memberships {
        let url = format!(
            "{}/api/v1/public/council?councilId={}",
            m.council_url.trim_end_matches('/'),
            urlencoding_query(&m.channel_auth_id),
        );
        let Ok(resp) = client.get(&url).send().await else {
            continue;
        };
        if !resp.status().is_success() {
            continue;
        }
        let Ok(body) = resp.json::<serde_json::Value>().await else {
            continue;
        };
        let Some(chs) = body
            .get("data")
            .and_then(|d| d.get("channels"))
            .and_then(|c| c.as_array())
        else {
            continue;
        };
        for ch in chs {
            let Some(id) = ch.get("channelContractId").and_then(|v| v.as_str()) else {
                continue;
            };
            let status = ch.get("status").and_then(|v| v.as_str()).unwrap_or("");
            let is_disabled = status == "disabled";
            channels.apply_state(id, is_disabled, None).await.ok();
        }
    }
    Ok(())
}

/// Minimal URL-encoder — match council.rs's local helper without pulling
/// in a heavier dependency just for this one call.
fn urlencoding_query(s: &str) -> String {
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

/// Extract the network-reported minimum-retained ledger from a Soroban
/// "startLedger must be within the ledger range: X - Y" error so the watcher
/// can re-seed past genesis when the cursor falls out of retention.
fn parse_min_ledger_from_error(err: &str) -> Option<u32> {
    let needle = "ledger range: ";
    let idx = err.find(needle)?;
    let after = &err[idx + needle.len()..];
    let dash = after.find(" -")?;
    after[..dash].trim().parse().ok()
}

async fn apply_event(
    repo: &CouncilMembershipRepo,
    channels: &ChannelStateRepo,
    channel_auth_id: &str,
    event: &EventResponse,
    events: &EventBroadcaster,
) -> anyhow::Result<()> {
    let topics = event.topic();
    let Some(first) = topics.first() else {
        return Ok(());
    };
    let topic = topic_symbol(first);
    match topic.as_deref() {
        Some("provider_added") => {
            repo.set_status(channel_auth_id, CouncilMembershipStatus::Active)
                .await?;
            events.send(ProviderEvent::channel_provider_added(
                events.current_scope(),
                channel_auth_id,
            ));
        }
        Some("provider_removed") => {
            repo.set_status(channel_auth_id, CouncilMembershipStatus::Rejected)
                .await?;
        }
        // UC6: the council's quorum-authorized record that an asset channel was
        // enabled or disabled. Topic[1] is the privacy-channel contract id;
        // the body is the `enabled` bool. The standin mirrors the council
        // truth so bundle-submit can enforce withdraw-only on disabled.
        Some("channel_state_changed") => {
            let Some(channel_addr) = topics.get(1).and_then(topic_contract_strkey) else {
                warn!(?topics, "channel_state_changed missing channel topic");
                return Ok(());
            };
            let enabled = event_body_bool(event).unwrap_or(true);
            let ledger = i64::try_from(event.ledger).ok();
            info!(
                channel = %channel_addr,
                enabled,
                ledger = ?ledger,
                "channel_state_changed: applying council disable/enable"
            );
            channels
                .apply_state(&channel_addr, !enabled, ledger)
                .await?;
        }
        _ => {}
    }
    Ok(())
}

fn topic_symbol(val: &ScVal) -> Option<String> {
    match val {
        ScVal::Symbol(ScSymbol(s)) => Some(String::from_utf8_lossy(s.as_slice()).into_owned()),
        _ => None,
    }
}

/// Decode a contract-address topic to its strkey form (C…). Returns None for
/// any non-contract-address ScVal.
fn topic_contract_strkey(val: &ScVal) -> Option<String> {
    use soroban_client::xdr::ScAddress;
    match val {
        ScVal::Address(ScAddress::Contract(soroban_client::xdr::ContractId(hash))) => {
            Some(format!("{}", Contract(hash.0)))
        }
        _ => None,
    }
}

/// Pull the boolean event body. `ChannelStateChanged` is declared with
/// `data_format = "single-value"` and a single `bool` payload.
fn event_body_bool(event: &EventResponse) -> Option<bool> {
    match event.value() {
        ScVal::Bool(b) => Some(b),
        _ => None,
    }
}
