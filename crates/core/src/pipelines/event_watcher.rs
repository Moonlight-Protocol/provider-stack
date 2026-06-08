use crate::config::Config;
use provider_stack_persistence::{EventWatcherStateRepo, PgPool};
use std::sync::Arc;
use tokio::time::{interval, MissedTickBehavior};
use tracing::{debug, instrument};

const CURSOR_KEY: &str = "channel_auth_events";

/// Event watcher: polls Soroban `get_events` on the channel-auth contract for
/// `provider_added` / `provider_removed`. Cursor persisted in `event_watcher_state`.
///
/// **Status**: scaffold — RPC polling + membership status update port next.
#[instrument(skip_all, name = "pipeline.event_watcher")]
pub async fn run(config: Arc<Config>, pool: PgPool) {
    let repo = EventWatcherStateRepo::new(pool);
    let mut tick = interval(config.event_watcher_interval);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        debug!("event-watcher tick");
        let _cursor = repo.get(CURSOR_KEY).await.unwrap_or(None);
        // TODO: rpc.get_events(filter, cursor) → for each: update council_memberships status,
        // persist new cursor via repo.set(CURSOR_KEY, new_cursor).
    }
}
