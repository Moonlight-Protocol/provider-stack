//! Event watcher pipeline.
//!
//! For every non-deleted council membership, poll Soroban `getEvents` on that membership's
//! channel-auth contract. On a `provider_added` event whose payload references this stack's
//! PP, promote the membership status to `ACTIVE`. On `provider_removed` mark it `REJECTED`.
//!
//! The cursor (last-seen-ledger) is per-channel-auth-id in `event_watcher_state`.

use crate::config::Config;
use crate::events::{EventBroadcaster, ProviderEvent};
use provider_stack_persistence::{
    CouncilMembershipRepo, CouncilMembershipStatus, EventWatcherStateRepo, PgPool,
};
use soroban_client::soroban_rpc::{EventResponse, EventType};
use soroban_client::xdr::{ScSymbol, ScVal};
use soroban_client::{EventFilter, Options, Pagination, Server, Topic};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{interval, MissedTickBehavior};
use tracing::{debug, instrument, warn};

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

    let mut tick = interval(config.event_watcher_interval);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let processing = Arc::new(Mutex::new(false));

    loop {
        tick.tick().await;
        let mut guard = processing.lock().await;
        if *guard {
            continue;
        }
        *guard = true;
        drop(guard);

        if let Err(e) = run_tick(&server, &pool, &events).await {
            warn!(error = %e, "event_watcher tick failed");
        }
        debug!("event_watcher tick complete");

        let mut guard = processing.lock().await;
        *guard = false;
    }
}

/// One watcher tick. Exposed for the integration test.
pub async fn run_tick(
    server: &Server,
    pool: &PgPool,
    events: &EventBroadcaster,
) -> anyhow::Result<()> {
    let memberships_repo = CouncilMembershipRepo::new(pool.clone());
    let cursor_repo = EventWatcherStateRepo::new(pool.clone());
    let memberships = memberships_repo.list_active().await?;

    for m in memberships {
        let cursor_key = format!("channel_auth:{}", m.channel_auth_id);
        let last_cursor = cursor_repo.get(&cursor_key).await?;
        let pagination = match last_cursor.as_deref() {
            Some(c) => Pagination::Cursor(c.to_string()),
            None => {
                // No cursor yet — seed from the network's current ledger, with a
                // safety lookback so we don't miss events emitted between when the
                // membership row was inserted and the watcher's first tick on it.
                let latest = server.get_latest_ledger().await?;
                Pagination::From(latest.sequence.saturating_sub(1000).max(1))
            }
        };
        let filter = EventFilter::new(EventType::Contract)
            .contract(&m.channel_auth_id)
            .topic(vec![Topic::Any, Topic::Any]);
        let result = server.get_events(pagination, vec![filter], EVENT_LIMIT).await;
        let response = match result {
            Ok(r) => r,
            Err(e) => {
                warn!(channel = %m.channel_auth_id, error = ?e, "get_events failed");
                continue;
            }
        };

        for event in &response.events {
            apply_event(&memberships_repo, &m.channel_auth_id, event, events).await?;
        }
        if let Some(new_cursor) = response.cursor {
            cursor_repo.set(&cursor_key, &new_cursor).await?;
        }
    }
    Ok(())
}

async fn apply_event(
    repo: &CouncilMembershipRepo,
    channel_auth_id: &str,
    event: &EventResponse,
    events: &EventBroadcaster,
) -> anyhow::Result<()> {
    let topics = event.topic();
    let Some(first) = topics.first() else {
        return Ok(());
    };
    let topic = topic_symbol(first);
    let new_status = match topic.as_deref() {
        Some("provider_added") => CouncilMembershipStatus::Active,
        Some("provider_removed") => CouncilMembershipStatus::Rejected,
        _ => return Ok(()),
    };
    repo.set_status(channel_auth_id, new_status).await?;
    if topic.as_deref() == Some("provider_added") {
        events.send(ProviderEvent::channel_provider_added(
            events.current_scope(),
            channel_auth_id,
        ));
    }
    Ok(())
}

fn topic_symbol(val: &ScVal) -> Option<String> {
    match val {
        ScVal::Symbol(ScSymbol(s)) => Some(String::from_utf8_lossy(s.as_slice()).into_owned()),
        _ => None,
    }
}
