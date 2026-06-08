use provider_stack_core::auth::sep43::NonceStore;
use provider_stack_core::config::Config;
use provider_stack_core::events::EventBroadcaster;
use provider_stack_persistence::PgPool;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub pool: PgPool,
    pub events: EventBroadcaster,
    pub nonces: Arc<NonceStore>,
}
