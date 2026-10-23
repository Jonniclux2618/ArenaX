use std::collections::HashMap;

/// Version of the rating formula. Bump whenever the math or tie-breaking
/// changes so that stored snapshots can be identified and rebuilt.
pub const RATING_VERSION: u32 = 1;

/// Base Elo K-factor used by the rating formula.
const K_FACTOR: f64 = 32.0;

/// A finalized match event is the single source of truth for rating changes.
/// `event_id` uniquely identifies the finalized match so that duplicate
/// deliveries can be detected and ignored.
#[derive(Debug, Clone, PartialEq)]
pub struct FinalizedMatchEvent {
    pub event_id: String,
    pub winner_id: String,
    pub loser_id: String,
}

/// Auditable record of a single rating change, capturing the before/after
/// values so corrections can be traced.
#[derive(Debug, Clone, PartialEq)]
pub struct RatingChange {
    pub event_id: String,
    pub player_id: String,
    pub before: f64,
    pub after: f64,
}

/// Result of applying an event: the new ratings plus the audit trail.
#[derive(Debug, Clone, PartialEq)]
#[derive(Default)]
pub struct ApplyOutcome {
    pub applied: bool,
    pub changes: Vec<RatingChange>,
}

/// In-memory leaderboard state keyed by player id.
#[derive(Debug, Clone, Default)]
pub struct Leaderboard {
    ratings: HashMap<String, f64>,
    /// Set of event ids already applied, used for idempotency.
    applied_events: std::collections::HashSet<String>,
}

impl Leaderboard {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn rating(&self, player_id: &str) -> f64 {
        *self.ratings.get(player_id).unwrap_or(&1500.0)
    }

    /// Apply a finalized match event incrementally. Duplicate events (same
    /// `event_id`) are ignored so rating changes are never applied twice.
    pub fn apply_event(&mut self, event: &FinalizedMatchEvent) -> ApplyOutcome {
        if self.applied_events.contains(&event.event_id) {
            return ApplyOutcome::default();
        }

        let winner_before = self.rating(&event.winner_id);
        let loser_before = self.rating(&event.loser_id);

        let (winner_after, loser_after) =
            compute_ratings(winner_before, loser_before);

        self.ratings.insert(event.winner_id.clone(), winner_after);
        self.ratings.insert(event.loser_id.clone(), loser_after);
        self.applied_events.insert(event.event_id.clone());

        ApplyOutcome {
            applied: true,
            changes: vec![
                RatingChange {
                    event_id: event.event_id.clone(),
                    player_id: event.winner_id.clone(),
                    before: winner_before,
                    after: winner_after,
                },
                RatingChange {
                    event_id: event.event_id.clone(),
                    player_id: event.loser_id.clone(),
                    before: loser_before,
                    after: loser_after,
                },
            ],
        }
    }

    /// Deterministic ranking snapshot: sorted by rating descending, then by
    /// player id ascending to break ties reproducibly.
    pub fn snapshot(&self) -> Vec<(String, f64)> {
        let mut entries: Vec<(String, f64)> = self
            .ratings
            .iter()
            .map(|(id, rating)| (id.clone(), *rating))
            .collect();
        entries.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        entries
    }
}

/// Full rebuild from authoritative match history. Produces the same snapshot
/// as incremental processing because it reuses the same idempotent apply path.
pub fn rebuild(history: &[FinalizedMatchEvent]) -> Leaderboard {
    let mut leaderboard = Leaderboard::new();
    for event in history {
        leaderboard.apply_event(event);
    }
    leaderboard
}

/// Versioned Elo rating formula. Returns (winner_after, loser_after).
fn compute_ratings(winner: f64, loser: f64) -> (f64, f64) {
    let expected_winner = 1.0 / (1.0 + 10f64.powf((loser - winner) / 400.0));
    let expected_loser = 1.0 - expected_winner;

    let winner_after = winner + K_FACTOR * (1.0 - expected_winner);
    let loser_after = loser + K_FACTOR * (0.0 - expected_loser);
    (winner_after, loser_after)
}
