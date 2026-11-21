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

/// Player-safe explanation of a single rating delta. Every field is derived
/// from persisted inputs (the finalized match, the rating version, and the
/// before/after values) so the explanation stays stable after future model
/// changes and never exposes confidential anti-abuse or moderation signals.
#[derive(Debug, Clone, PartialEq)]
pub struct RatingExplanation {
    /// Finalized match this delta belongs to.
    pub event_id: String,
    /// Rating formula version that produced the delta.
    pub rating_version: u32,
    /// Player whose rating changed.
    pub player_id: String,
    /// Rating before the match was applied.
    pub before: f64,
    /// Rating after the match was applied.
    pub after: f64,
    /// Signed delta (`after - before`).
    pub delta: f64,
    /// Concise, player-safe contributing factors.
    pub factors: Vec<RatingFactor>,
}

/// A single player-safe contributing factor behind a rating delta.
#[derive(Debug, Clone, PartialEq)]
pub struct RatingFactor {
    pub label: String,
    pub detail: String,
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
    /// Persisted explanations keyed by (event_id, player_id). Stored at apply
    /// time so later reads never recompute against a changed model.
    explanations: HashMap<(String, String), RatingExplanation>,
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

        let winner_change = RatingChange {
            event_id: event.event_id.clone(),
            player_id: event.winner_id.clone(),
            before: winner_before,
            after: winner_after,
        };
        let loser_change = RatingChange {
            event_id: event.event_id.clone(),
            player_id: event.loser_id.clone(),
            before: loser_before,
            after: loser_after,
        };

        // Persist explanations from the same inputs used to compute the delta.
        self.explanations.insert(
            (event.event_id.clone(), event.winner_id.clone()),
            explain_change(&winner_change, true),
        );
        self.explanations.insert(
            (event.event_id.clone(), event.loser_id.clone()),
            explain_change(&loser_change, false),
        );

        ApplyOutcome {
            applied: true,
            changes: vec![winner_change, loser_change],
        }
    }

    /// Retrieve the persisted, player-safe explanation for a rating delta.
    /// Returns `None` when the event was never applied (or was a duplicate).
    pub fn explain(&self, event_id: &str, player_id: &str) -> Option<&RatingExplanation> {
        self.explanations
            .get(&(event_id.to_string(), player_id.to_string()))
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

/// Build a player-safe explanation from a persisted rating change. Only the
/// finalized match id, rating version, and before/after values are used, so
/// the result is reproducible and free of confidential signals.
fn explain_change(change: &RatingChange, won: bool) -> RatingExplanation {
    let delta = change.after - change.before;
    let factors = vec![
        RatingFactor {
            label: "match_result".to_string(),
            detail: if won {
                "Won the finalized match".to_string()
            } else {
                "Lost the finalized match".to_string()
            },
        },
        RatingFactor {
            label: "rating_before".to_string(),
            detail: format!("Rating before match: {:.2}", change.before),
        },
        RatingFactor {
            label: "rating_version".to_string(),
            detail: format!("Computed with rating version {}", RATING_VERSION),
        },
    ];

    RatingExplanation {
        event_id: change.event_id.clone(),
        rating_version: RATING_VERSION,
        player_id: change.player_id.clone(),
        before: change.before,
        after: change.after,
        delta,
        factors,
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
