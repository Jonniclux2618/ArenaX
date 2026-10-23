use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Version of the rating formula. Bump whenever the math or tie-breaking
/// changes so that snapshots can be rebuilt deterministically per version.
pub const RATING_FORMULA_VERSION: u32 = 1;

/// The authoritative, source-of-truth record of a finalized match.
///
/// Leaderboard and player-stat services must derive all rating changes from
/// these events only. Each event carries a stable `event_id` so that duplicate
/// deliveries can be detected and ignored (idempotent incremental updates),
/// and a monotonic `sequence` so that a full rebuild from match history can be
/// replayed in the exact same order as incremental processing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MatchEvent {
    /// Stable identifier of this finalized-match event. Used for deduplication.
    pub event_id: Uuid,
    /// Identifier of the match this event finalizes.
    pub match_id: Uuid,
    /// Monotonic sequence assigned by the authoritative match store. Rebuilds
    /// replay events in ascending sequence order to reproduce rankings.
    pub sequence: i64,
    /// Participants and their scores as recorded by the match service.
    pub participants: Vec<MatchParticipant>,
    /// Rating formula version in effect when the match was finalized.
    pub rating_formula_version: u32,
    /// When the match was finalized (authoritative timestamp).
    pub finalized_at: DateTime<Utc>,
}

/// A single participant's authoritative result within a finalized match.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MatchParticipant {
    pub player_id: Uuid,
    /// Final score/placement for this participant in the match.
    pub score: i64,
}

impl MatchEvent {
    /// Deterministic ordering key for replay. Events are applied in ascending
    /// `sequence`, with `event_id` as a stable tie-breaker so that two events
    /// sharing a sequence still replay identically across rebuilds.
    pub fn ordering_key(&self) -> (i64, Uuid) {
        (self.sequence, self.event_id)
    }

    /// Participants in a deterministic order (by player id) so that rating
    /// deltas and tie-breaking do not depend on input ordering.
    pub fn ordered_participants(&self) -> Vec<&MatchParticipant> {
        let mut participants: Vec<&MatchParticipant> = self.participants.iter().collect();
        participants.sort_by_key(|p| p.player_id);
        participants
    }
}

/// Auditable record capturing the before/after rating state for a single
/// player when a match event is applied. Corrections append a new record
/// rather than mutating history, preserving an audit trail.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RatingChangeRecord {
    pub event_id: Uuid,
    pub match_id: Uuid,
    pub player_id: Uuid,
    pub rating_formula_version: u32,
    pub rating_before: i64,
    pub rating_after: i64,
    pub applied_at: DateTime<Utc>,
}

impl RatingChangeRecord {
    pub fn delta(&self) -> i64 {
        self.rating_after - self.rating_before
    }
}
