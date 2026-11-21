use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Lifecycle of a player's presence in the matchmaking queue.
///
/// The queue-exit state machine is deterministic:
/// - `Queued` -> `Cancelled` (player cancels) or `TimedOut` (queue wait exceeded)
/// - `Queued` -> `Matched` (a match was formed)
/// - `Matched` -> `Reserved` (reservation created for the match)
/// - `Matched` -> `TimedOut` (player failed to accept in time)
/// - `Reserved` -> `Expired` (reservation window elapsed without confirmation)
/// - `Reserved` -> `Confirmed` (player accepted the match)
///
/// Terminal states (`Cancelled`, `TimedOut`, `Expired`, `Confirmed`) are
/// immutable; a player must re-join to obtain a new queue entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueExitState {
    Queued,
    Matched,
    Reserved,
    Confirmed,
    Cancelled,
    TimedOut,
    Expired,
}

impl QueueExitState {
    /// Whether this state is terminal (no further transitions allowed).
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            QueueExitState::Confirmed
                | QueueExitState::Cancelled
                | QueueExitState::TimedOut
                | QueueExitState::Expired
        )
    }

    /// Whether the player still occupies a queue slot.
    pub fn occupies_queue(self) -> bool {
        matches!(self, QueueExitState::Queued)
    }

    /// Whether the player holds a reservation that must be cleaned up.
    pub fn holds_reservation(self) -> bool {
        matches!(self, QueueExitState::Matched | QueueExitState::Reserved)
    }

    /// Deterministic transition check used by the service boundary.
    pub fn can_transition_to(self, next: QueueExitState) -> bool {
        use QueueExitState::*;
        matches!(
            (self, next),
            (Queued, Cancelled)
                | (Queued, TimedOut)
                | (Queued, Matched)
                | (Matched, Reserved)
                | (Matched, TimedOut)
                | (Reserved, Confirmed)
                | (Reserved, Expired)
        )
    }
}

/// Reason a queue entry left the queue, surfaced to clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueExitReason {
    PlayerCancelled,
    QueueTimeout,
    MatchTimeout,
    ReservationExpired,
    MatchConfirmed,
}

/// A single queue entry. `id` is the idempotency key: retries of join/cancel
/// with the same `id` must not create duplicate entries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueEntry {
    pub id: Uuid,
    pub player_id: Uuid,
    pub state: QueueExitState,
    pub reason: Option<QueueExitReason>,
    pub enqueued_at: DateTime<Utc>,
    pub matched_at: Option<DateTime<Utc>>,
    pub reservation_expires_at: Option<DateTime<Utc>>,
    pub exited_at: Option<DateTime<Utc>>,
}

impl QueueEntry {
    pub fn new(id: Uuid, player_id: Uuid, now: DateTime<Utc>) -> Self {
        Self {
            id,
            player_id,
            state: QueueExitState::Queued,
            reason: None,
            enqueued_at: now,
            matched_at: None,
            reservation_expires_at: None,
            exited_at: None,
        }
    }

    /// Apply a transition, returning `false` if the transition is invalid or
    /// the entry is already terminal (idempotent no-op for retries).
    pub fn transition(
        &mut self,
        next: QueueExitState,
        reason: Option<QueueExitReason>,
        now: DateTime<Utc>,
    ) -> bool {
        if self.state == next {
            // Idempotent retry: no state change, no duplicate side effects.
            return false;
        }
        if !self.state.can_transition_to(next) {
            return false;
        }
        self.state = next;
        self.reason = reason;
        match next {
            QueueExitState::Matched => self.matched_at = Some(now),
            QueueExitState::Reserved => {
                self.reservation_expires_at = Some(now + chrono::Duration::seconds(30))
            }
            _ if next.is_terminal() => self.exited_at = Some(now),
            _ => {}
        }
        true
    }

    /// Whether a reservation has elapsed and should be expired by cleanup.
    pub fn reservation_expired(&self, now: DateTime<Utc>) -> bool {
        self.state == QueueExitState::Reserved
            && self
                .reservation_expires_at
                .map(|exp| now >= exp)
                .unwrap_or(false)
    }
}
