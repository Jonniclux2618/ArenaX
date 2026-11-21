//! Canonical matchmaking service boundary.
//!
//! Defines the deterministic queue-exit state machine for queued, matched,
//! and reserved players (issue #193). All join/cancel/timeout/reconnect
//! operations funnel through this module so that retries are idempotent and
//! reservation expiry restores eligible players without skipping fairness
//! rules.
//!
//! Issue #194: adaptive rating-window expansion is bounded by a versioned
//! per-ranked-mode queue policy. The service never exceeds the configured
//! maximum rating gap, expansion cadence, or region constraints, and records
//! the effective window on every match proposal.

use std::collections::HashMap;
use std::sync::Mutex;

/// Lifecycle of a player inside the matchmaking queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueState {
    /// Waiting in the queue, eligible to be matched.
    Queued,
    /// Placed into a match; awaiting reservation confirmation.
    Matched,
    /// Holds a reservation slot for a specific match.
    Reserved,
    /// Left the queue voluntarily (cancel).
    Cancelled,
    /// Reservation or queue slot expired (timeout).
    Expired,
}

impl QueueState {
    /// Terminal states cannot transition further without a fresh join.
    pub fn is_terminal(self) -> bool {
        matches!(self, QueueState::Cancelled | QueueState::Expired)
    }
}

/// Outcome surfaced to clients for every queue-exit operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueOutcome {
    /// Player is queued (join accepted or already queued).
    Queued { position: usize },
    /// Player was matched into a match.
    Matched { match_id: String },
    /// Player holds a reservation for a match.
    Reserved { match_id: String },
    /// Player cancelled; `already` is true when the cancel was a no-op retry.
    Cancelled { already: bool },
    /// Reservation/queue slot expired.
    Expired { already: bool },
    /// Operation rejected because the player is in a terminal state.
    Rejected { reason: RejectReason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// Cancel attempted after the player was already matched.
    AlreadyMatched,
    /// Reservation confirmation attempted without a reservation.
    NoReservation,
    /// Player is not known to the queue.
    UnknownPlayer,
}

/// Versioned per-ranked-mode queue policy bounding adaptive expansion.
///
/// `version` is bumped whenever any bound changes so operators can audit
/// which policy produced a given match proposal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuePolicy {
    /// Policy schema/config version (e.g. `"2024-06-01"`).
    pub version: String,
    /// Ranked mode identifier this policy applies to.
    pub mode: String,
    /// Hard ceiling on the rating gap between matched players.
    pub max_rating_gap: u32,
    /// Initial rating window before any expansion.
    pub initial_rating_gap: u32,
    /// Rating points added per expansion step.
    pub expansion_step: u32,
    /// Seconds of wait time between expansion steps.
    pub expansion_cadence_secs: u64,
    /// Allowed region identifiers; empty means no region restriction.
    pub allowed_regions: Vec<String>,
}

impl QueuePolicy {
    /// Effective rating window for a player who has waited `wait_secs`.
    ///
    /// Expansion is bounded by `max_rating_gap` and never exceeds it, even
    /// for arbitrarily long waits.
    pub fn effective_window(&self, wait_secs: u64) -> u32 {
        let steps = if self.expansion_cadence_secs == 0 {
            0
        } else {
            wait_secs / self.expansion_cadence_secs
        };
        let steps = u32::try_from(steps).unwrap_or(u32::MAX);
        let expanded = self
            .initial_rating_gap
            .saturating_add(self.expansion_step.saturating_mul(steps));
        expanded.min(self.max_rating_gap)
    }

    /// Whether two players may be matched under this policy given their
    /// ratings, regions, and the effective window.
    pub fn permits(&self, rating_a: u32, rating_b: u32, region_a: &str, region_b: &str, window: u32) -> bool {
        if !self.allowed_regions.is_empty()
            && (!self.allowed_regions.iter().any(|r| r == region_a)
                || !self.allowed_regions.iter().any(|r| r == region_b))
        {
            return false;
        }
        rating_a.abs_diff(rating_b) <= window.min(self.max_rating_gap)
    }
}

/// Recorded effective window for a match proposal (audit trail).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchProposal {
    pub match_id: String,
    pub policy_version: String,
    pub mode: String,
    /// Effective (expanded) rating window applied to this proposal.
    pub effective_window: u32,
    pub rating_gap: u32,
}

/// A single queue entry keyed by player id.
#[derive(Debug, Clone)]
struct Entry {
    state: QueueState,
    match_id: Option<String>,
    /// Monotonic sequence used to preserve fairness ordering.
    seq: u64,
}

/// Deterministic matchmaking queue with idempotent operations.
#[derive(Debug, Default)]
pub struct MatchmakingQueue {
    inner: Mutex<QueueInner>,
}

#[derive(Debug, Default)]
struct QueueInner {
    entries: HashMap<String, Entry>,
    next_seq: u64,
    /// Recorded effective windows for every match proposal.
    proposals: Vec<MatchProposal>,
}

impl MatchmakingQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Evaluate a match proposal under `policy` for two players who have
    /// waited `wait_secs`. The effective window is bounded by the policy and
    /// recorded on the proposal. Returns `None` when the policy forbids the
    /// pairing (rating gap or region constraint).
    pub fn propose_match(
        &self,
        match_id: &str,
        policy: &QueuePolicy,
        rating_a: u32,
        rating_b: u32,
        region_a: &str,
        region_b: &str,
        wait_secs: u64,
    ) -> Option<MatchProposal> {
        let window = policy.effective_window(wait_secs);
        if !policy.permits(rating_a, rating_b, region_a, region_b, window) {
            return None;
        }
        let proposal = MatchProposal {
            match_id: match_id.to_string(),
            policy_version: policy.version.clone(),
            mode: policy.mode.clone(),
            effective_window: window,
            rating_gap: rating_a.abs_diff(rating_b),
        };
        let mut inner = self.inner.lock().expect("matchmaking lock poisoned");
        inner.proposals.push(proposal.clone());
        Some(proposal)
    }

    /// Recorded match proposals, in creation order.
    pub fn proposals(&self) -> Vec<MatchProposal> {
        let inner = self.inner.lock().expect("matchmaking lock poisoned");
        inner.proposals.clone()
    }

    /// Join the queue. Retries are idempotent: an already-queued player keeps
    /// their original position and sequence.
    pub fn join(&self, player_id: &str) -> QueueOutcome {
        let mut inner = self.inner.lock().expect("matchmaking lock poisoned");
        if let Some(entry) = inner.entries.get(player_id) {
            match entry.state {
                QueueState::Queued => {
                    let position = inner.position_of(player_id);
                    return QueueOutcome::Queued { position };
                }
                QueueState::Matched => {
                    return QueueOutcome::Matched {
                        match_id: entry.match_id.clone().unwrap_or_default(),
                    };
                }
                QueueState::Reserved => {
                    return QueueOutcome::Reserved {
                        match_id: entry.match_id.clone().unwrap_or_default(),
                    };
                }
                QueueState::Cancelled | QueueState::Expired => {
                    // A fresh join after a terminal exit re-enters the queue
                    // with a new sequence, preserving fairness ordering.
                }
            }
        }
        let seq = inner.next_seq;
        inner.next_seq += 1;
        inner.entries.insert(
            player_id.to_string(),
            Entry {
                state: QueueState::Queued,
                match_id: None,
                seq,
            },
        );
        let position = inner.position_of(player_id);
        QueueOutcome::Queued { position }
    }

    /// Cancel a queued player. Cancelling a matched or reserved player is
    /// rejected so finalized matches stay immutable. Retries are idempotent.
    pub fn cancel(&self, player_id: &str) -> QueueOutcome {
        let mut inner = self.inner.lock().expect("matchmaking lock poisoned");
        match inner.entries.get_mut(player_id) {
            None => QueueOutcome::Rejected {
                reason: RejectReason::UnknownPlayer,
            },
            Some(entry) => match entry.state {
                QueueState::Queued => {
                    entry.state = QueueState::Cancelled;
                    QueueOutcome::Cancelled { already: false }
                }
                QueueState::Cancelled | QueueState::Expired => {
                    QueueOutcome::Cancelled { already: true }
                }
                QueueState::Matched | QueueState::Reserved => QueueOutcome::Rejected {
                    reason: RejectReason::AlreadyMatched,
                },
            },
        }
    }

    /// Expire a queued or reserved player. Expiry of a reserved player frees
    /// the reservation and returns the player to the queue tail so fairness
    /// ordering is preserved. Retries are idempotent.
    pub fn expire(&self, player_id: &str) -> QueueOutcome {
        let mut inner = self.inner.lock().expect("matchmaking lock poisoned");
        let seq = inner.next_seq;
        match inner.entries.get_mut(player_id) {
            None => QueueOutcome::Rejected {
                reason: RejectReason::UnknownPlayer,
            },
            Some(entry) => match entry.state {
                QueueState::Queued => {
                    entry.state = QueueState::Expired;
                    QueueOutcome::Expired { already: false }
                }
                QueueState::Reserved => {
                    // Reservation cleanup: restore eligibility at the tail.
                    entry.state = QueueState::Queued;
                    entry.match_id = None;
                    entry.seq = seq;
                    inner.next_seq += 1;
                    let position = inner.position_of(player_id);
                    QueueOutcome::Queued { position }
                }
                QueueState::Matched => QueueOutcome::Rejected {
                    reason: RejectReason::AlreadyMatched,
                },
                QueueState::Cancelled | QueueState::Expired => {
                    QueueOutcome::Expired { already: true }
                }
            },
        }
    }

    /// Mark a queued player as matched. Idempotent for the same match id.
    pub fn mark_matched(&self, player_id: &str, match_id: &str) -> QueueOutcome {
        let mut inner = self.inner.lock().expect("matchmaking lock poisoned");
        match inner.entries.get_mut(player_id) {
            None => QueueOutcome::Rejected {
                reason: RejectReason::UnknownPlayer,
            },
            Some(entry) => match entry.state {
                QueueState::Queued => {
                    entry.state = QueueState::Matched;
                    entry.match_id = Some(match_id.to_string());
                    QueueOutcome::Matched {
                        match_id: match_id.to_string(),
                    }
                }
                QueueState::Matched | QueueState::Reserved => QueueOutcome::Matched {
                    match_id: entry.match_id.clone().unwrap_or_default(),
                },
                QueueState::Cancelled | QueueState::Expired => QueueOutcome::Rejected {
                    reason: RejectReason::UnknownPlayer,
                },
            },
        }
    }

    /// Confirm a reservation for a matched player. Idempotent.
    pub fn reserve(&self, player_id: &str) -> QueueOutcome {
        let mut inner = self.inner.lock().expect("matchmaking lock poisoned");
        match inner.entries.get_mut(player_id) {
            None => QueueOutcome::Rejected {
                reason: RejectReason::UnknownPlayer,
            },
            Some(entry) => match entry.state {
                QueueState::Matched => {
                    entry.state = QueueState::Reserved;
                    QueueOutcome::Reserved {
                        match_id: entry.match_id.clone().unwrap_or_default(),
                    }
                }
                QueueState::Reserved => QueueOutcome::Reserved {
                    match_id: entry.match_id.clone().unwrap_or_default(),
                },
                QueueState::Queued => QueueOutcome::Rejected {
                    reason: RejectReason::NoReservation,
                },
                QueueState::Cancelled | QueueState::Expired => QueueOutcome::Rejected {
                    reason: RejectReason::UnknownPlayer,
                },
            },
        }
    }
}

impl QueueInner {
    /// Fairness position: number of queued entries with a lower sequence.
    fn position_of(&self, player_id: &str) -> usize {
        let seq = match self.entries.get(player_id) {
            Some(entry) => entry.seq,
            None => return 0,
        };
        self.entries
            .values()
            .filter(|e| e.state == QueueState::Queued && e.seq < seq)
            .count()
    }
}
