use crate::realtime::events::{channels, RealtimeEvent};
use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use tracing::{error, debug};
use uuid::Uuid;

/// Redis key holding the monotonic event sequence for one match (Issue #33).
fn match_event_seq_key(match_id: Uuid) -> String {
    format!("match:{}:event_seq", match_id)
}

/// Publishes domain events to Redis Pub/Sub channels.
/// Services inject this to emit real-time events.
#[derive(Clone)]
pub struct EventBus {
    redis: ConnectionManager,
}

impl EventBus {
    pub fn new(redis: ConnectionManager) -> Self {
        Self { redis }
    }

    /// Publish an event to a specific user's channel.
    pub async fn publish_to_user(&self, user_id: Uuid, event: &RealtimeEvent) {
        let channel = channels::user_channel(user_id);
        self.publish(&channel, event).await;
    }

    /// Publish an event to a specific match's channel.
    pub async fn publish_to_match(&self, match_id: Uuid, event: &RealtimeEvent) {
        let channel = channels::match_channel(match_id);
        self.publish(&channel, event).await;
    }

    /// Publish an event to an arbitrary named channel.
    ///
    /// Used for fan-out channels that are not keyed by a single id — the
    /// per-category leaderboard channels (Issue #900), for instance.
    pub async fn publish_to_channel(&self, channel: &str, event: &RealtimeEvent) {
        self.publish(channel, event).await;
    }

    /// Allocate the next monotonic sequence number for a match's event stream
    /// (Issue #33). Clients use it to detect duplicates, stale frames, and gaps.
    ///
    /// Returns `0` when the counter cannot be allocated (e.g. Redis outage) —
    /// callers publish the event as "unsequenced" rather than dropping it.
    pub async fn next_match_version(&self, match_id: Uuid) -> u64 {
        let key = match_event_seq_key(match_id);
        let mut conn = self.redis.clone();
        match conn.incr::<_, _, u64>(&key, 1u64).await {
            Ok(version) => version,
            Err(e) => {
                error!(match_id = %match_id, error = %e, "Failed to allocate match event version");
                0
            }
        }
    }

    /// Current sequence number of a match's event stream without allocating a
    /// new one. Returned on REST snapshots so a client can adopt the live
    /// stream's baseline after an authoritative fetch (`None` when no event
    /// has been published yet or Redis is unavailable).
    pub async fn current_match_version(&self, match_id: Uuid) -> Option<u64> {
        let key = match_event_seq_key(match_id);
        let mut conn = self.redis.clone();
        conn.get::<_, Option<u64>>(&key).await.unwrap_or(None)
    }

    /// Publish a serialized event to a Redis Pub/Sub channel.
    async fn publish(&self, channel: &str, event: &RealtimeEvent) {
        let payload = match serde_json::to_string(event) {
            Ok(json) => json,
            Err(e) => {
                error!(error = %e, "Failed to serialize event for Redis publish");
                return;
            }
        };

        let mut conn = self.redis.clone();
        if let Err(e) = conn.publish::<_, _, ()>(channel, &payload).await {
            error!(channel = %channel, error = %e, "Failed to publish event to Redis");
        } else {
            debug!(channel = %channel, "Event published to Redis");
        }
    }
}
