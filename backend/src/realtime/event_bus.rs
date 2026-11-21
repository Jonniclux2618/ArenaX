use crate::realtime::events::{channels, RealtimeEvent};
use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use tracing::{error, debug};
use uuid::Uuid;

/// How long a match's event sequence counter is retained in Redis. Once the
/// counter expires, any client holding an older watermark must treat its
/// stream as expired and refetch an authoritative snapshot (Issue #239).
const MATCH_EVENT_SEQ_TTL_SECS: u64 = 60 * 60 * 24;

/// Redis key holding the monotonic event sequence for one match (Issue #33).
fn match_event_seq_key(match_id: Uuid) -> String {
    format!("match:{}:event_seq", match_id)
}

/// Redis key holding the monotonic event sequence for one user's stream.
fn user_event_seq_key(user_id: Uuid) -> String {
    format!("user:{}:event_seq", user_id)
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
    ///
    /// The event is stamped with the next sequence number of the user's stream
    /// so clients can detect gaps, duplicates, and stale frames (Issue #239).
    pub async fn publish_to_user(&self, user_id: Uuid, event: &RealtimeEvent) {
        let channel = channels::user_channel(user_id);
        let seq = self.next_user_version(user_id).await;
        self.publish(&channel, event, seq).await;
    }

    /// Publish an event to a specific match's channel.
    ///
    /// The event is stamped with the next sequence number of the match's
    /// stream so clients can detect gaps, duplicates, and stale frames
    /// (Issue #239).
    pub async fn publish_to_match(&self, match_id: Uuid, event: &RealtimeEvent) {
        let channel = channels::match_channel(match_id);
        let seq = self.next_match_version(match_id).await;
        self.publish(&channel, event, seq).await;
    }

    /// Publish an event to an arbitrary named channel.
    ///
    /// Used for fan-out channels that are not keyed by a single id — the
    /// per-category leaderboard channels (Issue #900), for instance.
    pub async fn publish_to_channel(&self, channel: &str, event: &RealtimeEvent) {
        // Fan-out channels are not keyed by a single id, so there is no
        // per-stream counter to allocate; publish unsequenced.
        self.publish(channel, event, 0).await;
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
            Ok(version) => {
                // Refresh the retention window on every allocation so an
                // active stream never expires mid-flight.
                if let Err(e) = conn
                    .expire::<_, ()>(&key, MATCH_EVENT_SEQ_TTL_SECS as i64)
                    .await
                {
                    debug!(match_id = %match_id, error = %e, "Failed to refresh match event seq TTL");
                }
                version
            }
            Err(e) => {
                error!(match_id = %match_id, error = %e, "Failed to allocate match event version");
                0
            }
        }
    }

    /// Allocate the next monotonic sequence number for a user's event stream
    /// (Issue #239). Returns `0` when the counter cannot be allocated.
    pub async fn next_user_version(&self, user_id: Uuid) -> u64 {
        let key = user_event_seq_key(user_id);
        let mut conn = self.redis.clone();
        match conn.incr::<_, _, u64>(&key, 1u64).await {
            Ok(version) => {
                if let Err(e) = conn
                    .expire::<_, ()>(&key, MATCH_EVENT_SEQ_TTL_SECS as i64)
                    .await
                {
                    debug!(user_id = %user_id, error = %e, "Failed to refresh user event seq TTL");
                }
                version
            }
            Err(e) => {
                error!(user_id = %user_id, error = %e, "Failed to allocate user event version");
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

    /// Current sequence number of a user's event stream without allocating a
    /// new one. Returned on REST snapshots so a client can adopt the live
    /// stream's baseline after an authoritative fetch (Issue #239).
    pub async fn current_user_version(&self, user_id: Uuid) -> Option<u64> {
        let key = user_event_seq_key(user_id);
        let mut conn = self.redis.clone();
        conn.get::<_, Option<u64>>(&key).await.unwrap_or(None)
    }

    /// Publish a serialized event to a Redis Pub/Sub channel.
    ///
    /// `seq` is the stream sequence number stamped onto the frame so clients
    /// can order, deduplicate, and gap-detect events (Issue #239). A value of
    /// `0` marks the frame as unsequenced.
    async fn publish(&self, channel: &str, event: &RealtimeEvent, seq: u64) {
        let payload = match serde_json::to_string(&SequencedEvent { seq, event }) {
            Ok(json) => json,
            Err(e) => {
                error!(error = %e, "Failed to serialize event for Redis publish");
                return;
            }
        };

        let mut conn = self.redis.clone();
        // Keep a bounded Redis Stream mirror for reconnect recovery. Pub/Sub
        // remains the live fan-out transport; the stream is authoritative for
        // replaying only the bounded, ordered tail after a subscriber reconnect.
        let stream_key = format!("realtime-stream:{}", channel);
        let stream_result = redis::cmd("XADD")
            .arg(&stream_key)
            .arg("MAXLEN")
            .arg("~")
            .arg(1000)
            .arg("*")
            .arg("payload")
            .arg(&payload)
            .query_async::<_, String>(&mut conn)
            .await;
        if let Err(e) = stream_result {
            error!(channel = %channel, error = %e, "Failed to append event to Redis Stream");
        }
        if let Err(e) = conn.publish::<_, _, ()>(channel, &payload).await {
            error!(channel = %channel, error = %e, "Failed to publish event to Redis");
        } else {
            debug!(channel = %channel, seq = seq, "Event published to Redis");
        }
    }
}

/// Wire envelope wrapping a realtime event with its stream sequence number.
///
/// Clients use `seq` to reject stale frames, drop duplicates, and detect gaps
/// (a gap or an expired stream means the client must refetch a snapshot).
#[derive(serde::Serialize)]
struct SequencedEvent<'a> {
    seq: u64,
    event: &'a RealtimeEvent,
}
