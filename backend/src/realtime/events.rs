use actix::Message;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// All real-time events pushed to clients over WebSocket connections.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RealtimeEvent {
    BalanceUpdate {
        user_id: Uuid,
        balance_ngn: i64,
        balance_arenax_tokens: i64,
        balance_xlm: i64,
        timestamp: String,
        /// Stream sequence allocated by the event bus. See [`StreamMeta`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stream_seq: Option<u64>,
    },
    MatchFound {
        match_id: Uuid,
        opponent_id: Uuid,
        opponent_name: String,
        game_mode: String,
        timestamp: String,
        /// Stream sequence allocated by the event bus. See [`StreamMeta`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stream_seq: Option<u64>,
    },
    MatchStatusChange {
        match_id: Uuid,
        from_status: String,
        to_status: String,
        timestamp: String,
        /// Stream sequence allocated by the event bus. See [`StreamMeta`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stream_seq: Option<u64>,
    },
    /// Sequence-aware match state event (Issue #33).
    ///
    /// Carries the full authoritative slice of match state (status, scores,
    /// winner) so a client can converge from a single ordered stream without
    /// refetching per event.
    ///
    /// # Client merge contract
    ///
    /// The client tracks the last `version` applied for this `match_id`:
    ///
    /// 1. `version <= last_seen` — duplicate or stale delivery; discard.
    ///    Reordered frames can never roll status, scores, or dispute state
    ///    backwards.
    /// 2. `version == last_seen + 1` — apply the payload onto the local
    ///    query-cache snapshot.
    /// 3. `version >  last_seen + 1` — one or more events were missed;
    ///    discard the frame, suppress the live stream, and re-fetch the
    ///    authoritative match snapshot from REST. The snapshot's `version`
    ///    becomes the new baseline (see `MatchResponse::version`).
    /// 4. `version == 0` — the sequencer could not be allocated (e.g. Redis
    ///    unavailable); treat the event as unsequenced and apply without
    ///    moving the baseline.
    MatchStateSync {
        match_id: Uuid,
        /// Monotonic per-match sequence number. See the merge contract.
        version: u64,
        /// Domain event name, e.g. `score_reported`, `disputed`,
        /// `conflict_detected`, `status_changed`.
        event: String,
        /// Match status after the change (`pending`, `in_progress`, ...).
        status: String,
        score_player1: Option<i32>,
        score_player2: Option<i32>,
        winner_id: Option<Uuid>,
        /// Human-readable detail (dispute reason, conflict description, ...).
        detail: Option<String>,
        timestamp: String,
        /// Stream sequence allocated by the event bus. See [`StreamMeta`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stream_seq: Option<u64>,
    },
    Notification {
        id: Uuid,
        title: String,
        body: String,
        category: String,
        timestamp: String,
        /// Stream sequence allocated by the event bus. See [`StreamMeta`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stream_seq: Option<u64>,
    },
    MatchCompleted {
        match_id: Uuid,
        winner_id: Uuid,
        elo_change: i32,
        timestamp: String,
        /// Stream sequence allocated by the event bus. See [`StreamMeta`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stream_seq: Option<u64>,
    },
    MatchDisputed {
        match_id: Uuid,
        reason: String,
        timestamp: String,
        /// Stream sequence allocated by the event bus. See [`StreamMeta`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stream_seq: Option<u64>,
    },
    MatchmakingMetricsUpdate {
        dashboard: serde_json::Value,
        timestamp: String,
        /// Stream sequence allocated by the event bus. See [`StreamMeta`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stream_seq: Option<u64>,
    },
    /// Incremental leaderboard update (Issue #900).
    ///
    /// Carries only the entries whose position moved, never the whole board.
    ///
    /// # Client merge contract
    ///
    /// The client keeps its own copy of the board and patches it:
    ///
    /// 1. If `version` is not exactly one greater than the last version seen
    ///    for this `category`, a delta was missed — discard the local board and
    ///    re-fetch it from the REST endpoint. Patching across a gap silently
    ///    corrupts the board, so a gap must never be applied.
    /// 2. For each change, replace the entry for `user_id` with the new
    ///    `ranking` and `elo_rating`, inserting it if the player was not on the
    ///    board before (`previous_ranking` is `None`).
    /// 3. Re-sort by `ranking` ascending.
    ///
    /// `previous_ranking` is informational — it is what the client last saw,
    /// which is what makes a "moved up 3 places" animation correct even when
    /// several moves were coalesced into one change.
    LeaderboardDelta {
        category: String,
        /// Monotonic per-category sequence number. See the merge contract.
        version: u64,
        changes: Vec<RankChange>,
        timestamp: String,
        /// Stream sequence allocated by the event bus. See [`StreamMeta`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stream_seq: Option<u64>,
    },
}

impl RealtimeEvent {
    /// The stream sequence carried by this event, if the bus allocated one.
    ///
    /// Events built by producers (before the bus stamps them) return `None`;
    /// events delivered to clients always carry `Some(seq)`.
    pub fn stream_seq(&self) -> Option<u64> {
        match self {
            RealtimeEvent::BalanceUpdate { stream_seq, .. }
            | RealtimeEvent::MatchFound { stream_seq, .. }
            | RealtimeEvent::MatchStatusChange { stream_seq, .. }
            | RealtimeEvent::MatchStateSync { stream_seq, .. }
            | RealtimeEvent::Notification { stream_seq, .. }
            | RealtimeEvent::MatchCompleted { stream_seq, .. }
            | RealtimeEvent::MatchDisputed { stream_seq, .. }
            | RealtimeEvent::MatchmakingMetricsUpdate { stream_seq, .. }
            | RealtimeEvent::LeaderboardDelta { stream_seq, .. } => *stream_seq,
        }
    }

    /// Stamp this event with the sequence allocated by the event bus.
    pub fn set_stream_seq(&mut self, seq: u64) {
        match self {
            RealtimeEvent::BalanceUpdate { stream_seq, .. }
            | RealtimeEvent::MatchFound { stream_seq, .. }
            | RealtimeEvent::MatchStatusChange { stream_seq, .. }
            | RealtimeEvent::MatchStateSync { stream_seq, .. }
            | RealtimeEvent::Notification { stream_seq, .. }
            | RealtimeEvent::MatchCompleted { stream_seq, .. }
            | RealtimeEvent::MatchDisputed { stream_seq, .. }
            | RealtimeEvent::MatchmakingMetricsUpdate { stream_seq, .. }
            | RealtimeEvent::LeaderboardDelta { stream_seq, .. } => *stream_seq = Some(seq),
        }
    }

    /// The match this event belongs to, when it is match-scoped.
    ///
    /// Used by the bus to route events onto the right per-match stream.
    pub fn match_id(&self) -> Option<Uuid> {
        match self {
            RealtimeEvent::MatchFound { match_id, .. }
            | RealtimeEvent::MatchStatusChange { match_id, .. }
            | RealtimeEvent::MatchStateSync { match_id, .. }
            | RealtimeEvent::MatchCompleted { match_id, .. }
            | RealtimeEvent::MatchDisputed { match_id, .. } => Some(*match_id),
            _ => None,
        }
    }
}

/// Sequencing metadata attached to every event the bus delivers.
///
/// A client bootstraps from a REST snapshot that carries a `watermark` (the
/// highest `stream_seq` reflected in that snapshot). It then applies live
/// events only when `stream_seq == last_applied + 1`; anything lower is stale
/// and anything higher is a gap that forces a fresh snapshot.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct StreamMeta {
    /// Monotonic sequence number allocated by the event bus.
    pub stream_seq: u64,
    /// Highest sequence reflected in the snapshot the client bootstrapped from.
    pub watermark: u64,
}

/// One player's movement on the leaderboard.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RankChange {
    pub user_id: Uuid,
    pub ranking: i32,
    /// The rank the client last saw, or `None` for a player entering the board.
    pub previous_ranking: Option<i32>,
    pub elo_rating: i32,
}

/// Envelope wrapping a realtime event for WebSocket delivery.
///
/// `stream_seq` is the sequence the bus allocated for this frame; `watermark`
/// is the highest sequence the server has published so far. Clients compare
/// `stream_seq` against their last applied sequence to detect gaps and stale
/// frames, and use `watermark` to decide whether a snapshot is current.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WsEnvelope {
    pub event: RealtimeEvent,
    #[serde(default)]
    pub stream_seq: u64,
    #[serde(default)]
    pub watermark: u64,
}

impl WsEnvelope {
    /// Build an envelope, taking the sequence from the event itself.
    pub fn new(event: RealtimeEvent, watermark: u64) -> Self {
        let stream_seq = event.stream_seq().unwrap_or_default();
        Self {
            event,
            stream_seq,
            watermark,
        }
    }
}

/// Messages received from the client over WebSocket.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Ping,
    Pong,
    Subscribe {
        channel: String,
    },
    Unsubscribe {
        channel: String,
    },
    Publish {
        channel: String,
        event: RealtimeEvent,
    },
    /// Ask the server to re-send a snapshot because the client detected a gap.
    ResyncRequest {
        channel: String,
        /// Last sequence the client successfully applied.
        last_applied_seq: u64,
    },
}

/// Actix message for delivering a realtime event to an actor.
#[derive(Debug, Clone, Message)]
#[rtype(result = "()")]
pub struct DeliverEvent(pub RealtimeEvent);

/// Force one or all sessions for a user to close after credential revocation.
#[derive(Debug, Clone, Message)]
#[rtype(result = "()")]
pub struct CredentialRevoked(pub Uuid, pub Option<String>);

/// Actix message for delivering a sequenced envelope to an actor.
#[derive(Debug, Clone, Message)]
#[rtype(result = "()")]
pub struct DeliverEnvelope(pub WsEnvelope);

/// Channel naming helpers for pub/sub routing.
pub mod channels {
    use uuid::Uuid;

    pub const USER_CHANNEL_PATTERN: &str = "user:*";
    pub const MATCH_CHANNEL_PATTERN: &str = "match:*";
    pub const LEADERBOARD_CHANNEL_PATTERN: &str = "leaderboard:*";
    pub const MATCHMAKING_METRICS_CHANNEL: &str = "matchmaking:metrics";

    pub fn user_channel(user_id: Uuid) -> String {
        format!("user:{}", user_id)
    }

    pub fn match_channel(match_id: Uuid) -> String {
        format!("match:{}", match_id)
    }

    /// Channel carrying leaderboard deltas for one game category.
    ///
    /// Per-category rather than one global channel: a client watching the FIFA
    /// board has no use for Call of Duty rank churn, and fanning both out to
    /// everyone is the cost this whole delta scheme exists to avoid.
    pub fn leaderboard_channel(category: &str) -> String {
        format!("leaderboard:{}", category)
    }
}

/// Sequence allocation helpers shared by the event bus and snapshot handlers.
///
/// The bus owns the authoritative counters; snapshot handlers read the current
/// value to stamp their response `watermark` so a client can tell whether the
/// snapshot it received is newer than the events it has already applied.
pub mod sequencing {
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Global stream counter for events that are not scoped to a match.
    static GLOBAL_SEQ: AtomicU64 = AtomicU64::new(0);

    /// Allocate the next global stream sequence.
    pub fn next_stream_seq() -> u64 {
        GLOBAL_SEQ.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// The highest global stream sequence allocated so far.
    ///
    /// Snapshot handlers embed this as the response `watermark`.
    pub fn current_stream_seq() -> u64 {
        GLOBAL_SEQ.load(Ordering::SeqCst)
    }

    /// Allocate the next sequence for a specific match stream.
    ///
    /// Per-match counters keep match events ordered independently of unrelated
    /// traffic, so a busy global stream cannot make a match client see gaps.
    pub fn next_match_version(match_id: uuid::Uuid) -> u64 {
        use std::collections::HashMap;
        use std::sync::Mutex;
        use std::sync::OnceLock;

        static MATCH_SEQ: OnceLock<Mutex<HashMap<uuid::Uuid, u64>>> = OnceLock::new();
        let map = MATCH_SEQ.get_or_init(|| Mutex::new(HashMap::new()));
        let mut guard = map.lock().expect("match sequence mutex poisoned");
        let entry = guard.entry(match_id).or_insert(0);
        *entry += 1;
        *entry
    }

    /// The highest sequence allocated for a match stream.
    ///
    /// Match snapshot handlers embed this as the response `watermark`.
    pub fn current_match_version(match_id: uuid::Uuid) -> u64 {
        use std::collections::HashMap;
        use std::sync::Mutex;
        use std::sync::OnceLock;

        static MATCH_SEQ: OnceLock<Mutex<HashMap<uuid::Uuid, u64>>> = OnceLock::new();
        let map = MATCH_SEQ.get_or_init(|| Mutex::new(HashMap::new()));
        let guard = map.lock().expect("match sequence mutex poisoned");
        guard.get(&match_id).copied().unwrap_or(0)
    }
}
