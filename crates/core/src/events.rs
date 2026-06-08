//! In-process event broadcast for WebSocket subscribers.
//!
//! The verifier emits one event per confirmed operation; subscribers connected to
//! `/api/v1/providers/:pk/events/ws` receive them via this channel.

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProviderEvent {
    BundleAccepted { bundle_id: String },
    BundleCompleted { bundle_id: String, tx_hash: String },
    BundleFailed { bundle_id: String, reason: String },
    Deposit { account: String, amount: i64 },
    Withdraw { account: String, amount: i64 },
}

#[derive(Clone)]
pub struct EventBroadcaster {
    tx: broadcast::Sender<ProviderEvent>,
}

impl EventBroadcaster {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    pub fn send(&self, ev: ProviderEvent) {
        // ignore lagging subscribers
        let _ = self.tx.send(ev);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ProviderEvent> {
        self.tx.subscribe()
    }
}

impl Default for EventBroadcaster {
    fn default() -> Self {
        Self::new(256)
    }
}
