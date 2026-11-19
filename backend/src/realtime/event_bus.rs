use crate::realtime::events::{channels, PublishedEvent, RealtimeEvent};
use redis::aio::ConnectionManager;
use redis::{AsyncCommands, Script};
use tracing::{error, debug};
use uuid::Uuid;

const REALTIME_STREAM_MAX_LENGTH: usize = 1_000;
const REALTIME_STREAM_TTL_SECONDS: usize = 7 * 24 * 60 * 60;
const MAX_REPLAY_EVENTS: usize = 1_000;

fn channel_stream_key(channel: &str) -> String {
    format!("realtime:stream:{}", channel)
}

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
        let append_and_notify = Script::new(
            r#"
            local id = redis.call(
                'XADD', KEYS[1], 'MAXLEN', '~', ARGV[1], '*', 'payload', ARGV[2]
            )
            redis.call('EXPIRE', KEYS[1], ARGV[4])
            redis.call('PUBLISH', ARGV[3], id)
            return id
            "#,
        );
        let stream_id: String = match append_and_notify
            .key(channel_stream_key(channel))
            .arg(REALTIME_STREAM_MAX_LENGTH)
            .arg(&payload)
            .arg(channel)
            .arg(REALTIME_STREAM_TTL_SECONDS)
            .invoke_async(&mut conn)
            .await
        {
            Ok(id) => id,
            Err(e) => {
                error!(channel = %channel, error = %e, "Failed to persist and notify realtime event");
                return;
            }
        };

        debug!(channel = %channel, stream_id = %stream_id, "Event persisted and published to Redis");
    }

    /// Read events after a client-provided stream cursor. The cursor is
    /// channel-specific and bounded by the stream retention limit.
    pub async fn replay_after(
        &self,
        channel: &str,
        cursor: &str,
    ) -> Result<Vec<PublishedEvent>, String> {
        let key = channel_stream_key(channel);
        let mut conn = self.redis.clone();
        let oldest: Vec<(String, Vec<(String, String)>)> = redis::cmd("XRANGE")
            .arg(&key)
            .arg("-")
            .arg("+")
            .arg("COUNT")
            .arg(1)
            .query_async(&mut conn)
            .await
            .map_err(|error| error.to_string())?;

        if let Some((oldest_id, _)) = oldest.first() {
            if compare_stream_ids(cursor, oldest_id)? == std::cmp::Ordering::Less {
                return Err("event cursor is outside retained history; fetch the authoritative REST snapshot".to_string());
            }
        } else {
            return Err("event stream history is unavailable; fetch the authoritative REST snapshot".to_string());
        }

        let newest: Vec<(String, Vec<(String, String)>)> = redis::cmd("XREVRANGE")
            .arg(&key)
            .arg("+")
            .arg("-")
            .arg("COUNT")
            .arg(1)
            .query_async(&mut conn)
            .await
            .map_err(|error| error.to_string())?;
        let Some((newest_id, _)) = newest.first() else {
            return Err("event stream history is unavailable; fetch the authoritative REST snapshot".to_string());
        };
        if compare_stream_ids(cursor, newest_id)? == std::cmp::Ordering::Greater {
            return Err("event cursor is newer than retained history; fetch the authoritative REST snapshot".to_string());
        }

        let rows: Vec<(String, Vec<(String, String)>)> = redis::cmd("XRANGE")
            .arg(&key)
            .arg(format!("({cursor}"))
            .arg("+")
            .arg("COUNT")
            .arg(MAX_REPLAY_EVENTS + 1)
            .query_async(&mut conn)
            .await
            .map_err(|error| error.to_string())?;

        if rows.is_empty() {
            let exists: bool = conn.exists(&key).await.map_err(|error| error.to_string())?;
            if !exists {
                return Err("event stream history is unavailable; fetch the authoritative REST snapshot".to_string());
            }
        }

        if rows.len() > MAX_REPLAY_EVENTS {
            return Err("event backlog exceeds the replay limit; fetch the authoritative REST snapshot".to_string());
        }

        rows.into_iter()
            .map(|(stream_id, fields)| {
                let payload = fields
                    .into_iter()
                    .find_map(|(name, value)| (name == "payload").then_some(value))
                    .ok_or_else(|| format!("event stream entry {stream_id} has no payload"))?;
                let event = serde_json::from_str(&payload)
                    .map_err(|error| format!("invalid event stream entry {stream_id}: {error}"))?;
                Ok(PublishedEvent {
                    cursor: stream_id,
                    event,
                })
            })
            .collect()
    }
}

fn compare_stream_ids(left: &str, right: &str) -> Result<std::cmp::Ordering, String> {
    fn parse(value: &str) -> Result<(u64, u64), String> {
        let (milliseconds, sequence) = value
            .split_once('-')
            .ok_or_else(|| format!("invalid Redis Stream ID: {value}"))?;
        let milliseconds = milliseconds
            .parse()
            .map_err(|_| format!("invalid Redis Stream ID: {value}"))?;
        let sequence = sequence
            .parse()
            .map_err(|_| format!("invalid Redis Stream ID: {value}"))?;
        Ok((milliseconds, sequence))
    }

    Ok(parse(left)?.cmp(&parse(right)?))
}
