use std::collections::HashMap;

/// Version of the rating formula. Bump whenever the math or tie-breaking
/// changes so that stored snapshots can be identified and rebuilt.
pub const RATING_VERSION: u32 = 1;

/// Version of the calibration/placement parameters. Bump whenever the
/// uncertainty model or provisional placement changes so that stored
/// snapshots can be identified and rebuilt.
pub const CALIBRATION_VERSION: u32 = 1;

/// Base Elo K-factor used by the rating formula.
const K_FACTOR: f64 = 32.0;

/// Conservative provisional rating assigned to new accounts. Lower than the
/// established default so new players are not overrated before proving
/// themselves.
const PROVISIONAL_RATING: f64 = 1400.0;

/// Rating assigned to established players with no recorded history.
const DEFAULT_RATING: f64 = 1500.0;

/// Initial uncertainty (in rating points) for a brand new account. High
/// uncertainty means placement matches move the rating more aggressively.
const INITIAL_UNCERTAINTY: f64 = 350.0;

/// Floor for uncertainty. Uncertainty never drops below this so returning
/// players retain a small amount of calibration headroom.
const MIN_UNCERTAINTY: f64 = 30.0;

/// Ceiling for uncertainty. Bounds the model so a single match can never
/// produce an unbounded swing.
const MAX_UNCERTAINTY: f64 = 400.0;

/// Multiplier applied to the K-factor while a player is still provisional.
/// Larger swings let placement matches converge quickly.
const PROVISIONAL_K_MULTIPLIER: f64 = 2.0;

/// Number of eligible finalized matches required before a player is
/// considered established (no longer provisional).
const PLACEMENT_MATCHES: u32 = 10;

/// Fraction by which uncertainty shrinks per eligible finalized match.
const UNCERTAINTY_DECAY: f64 = 0.85;

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

/// Per-player calibration state: rating plus uncertainty and the count of
/// eligible finalized matches that have contributed to it.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerRating {
    pub rating: f64,
    pub uncertainty: f64,
    pub matches_played: u32,
}

impl PlayerRating {
    /// Conservative provisional state for a brand new account.
    pub fn provisional() -> Self {
        Self {
            rating: PROVISIONAL_RATING,
            uncertainty: clamp_uncertainty(INITIAL_UNCERTAINTY),
            matches_played: 0,
        }
    }

    /// Established state for a player with no recorded history.
    pub fn established() -> Self {
        Self {
            rating: DEFAULT_RATING,
            uncertainty: clamp_uncertainty(MIN_UNCERTAINTY),
            matches_played: PLACEMENT_MATCHES,
        }
    }

    pub fn is_provisional(&self) -> bool {
        self.matches_played < PLACEMENT_MATCHES
    }
}

/// In-memory leaderboard state keyed by player id.
#[derive(Debug, Clone, Default)]
pub struct Leaderboard {
    players: HashMap<String, PlayerRating>,
    /// Set of event ids already applied, used for idempotency.
    applied_events: std::collections::HashSet<String>,
}

impl Leaderboard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a brand new account with a conservative provisional rating
    /// and elevated uncertainty. Existing players are left untouched.
    pub fn register_new_player(&mut self, player_id: &str) {
        self.players
            .entry(player_id.to_string())
            .or_insert_with(PlayerRating::provisional);
    }

    pub fn rating(&self, player_id: &str) -> f64 {
        self.player(player_id).rating
    }

    pub fn uncertainty(&self, player_id: &str) -> f64 {
        self.player(player_id).uncertainty
    }

    fn player(&self, player_id: &str) -> PlayerRating {
        self.players
            .get(player_id)
            .cloned()
            .unwrap_or_else(PlayerRating::established)
    }

    /// Apply a finalized match event incrementally. Duplicate events (same
    /// `event_id`) are ignored so rating changes are never applied twice.
    /// Only finalized events reach this boundary; uncertainty decreases only
    /// as a result of applying such an event.
    pub fn apply_event(&mut self, event: &FinalizedMatchEvent) -> ApplyOutcome {
        if self.applied_events.contains(&event.event_id) {
            return ApplyOutcome::default();
        }

        let winner_before = self.player(&event.winner_id);
        let loser_before = self.player(&event.loser_id);

        let (winner_after, loser_after) = compute_ratings(&winner_before, &loser_before);

        self.players.insert(event.winner_id.clone(), winner_after.clone());
        self.players.insert(event.loser_id.clone(), loser_after.clone());
        self.applied_events.insert(event.event_id.clone());

        ApplyOutcome {
            applied: true,
            changes: vec![
                RatingChange {
                    event_id: event.event_id.clone(),
                    player_id: event.winner_id.clone(),
                    before: winner_before.rating,
                    after: winner_after.rating,
                },
                RatingChange {
                    event_id: event.event_id.clone(),
                    player_id: event.loser_id.clone(),
                    before: loser_before.rating,
                    after: loser_after.rating,
                },
            ],
        }
    }

    /// Deterministic ranking snapshot: sorted by rating descending, then by
    /// player id ascending to break ties reproducibly.
    pub fn snapshot(&self) -> Vec<(String, f64)> {
        let mut entries: Vec<(String, f64)> = self
            .players
            .iter()
            .map(|(id, player)| (id.clone(), player.rating))
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

/// Clamp uncertainty into the bounded calibration range.
fn clamp_uncertainty(value: f64) -> f64 {
    value.max(MIN_UNCERTAINTY).min(MAX_UNCERTAINTY)
}

/// Versioned Elo rating formula with uncertainty-aware placement. Returns the
/// updated winner and loser calibration states. Provisional players use a
/// larger effective K-factor so placement matches converge quickly, and
/// uncertainty decays only as eligible finalized matches are applied.
fn compute_ratings(winner: &PlayerRating, loser: &PlayerRating) -> (PlayerRating, PlayerRating) {
    let expected_winner =
        1.0 / (1.0 + 10f64.powf((loser.rating - winner.rating) / 400.0));
    let expected_loser = 1.0 - expected_winner;

    let winner_k = effective_k(winner);
    let loser_k = effective_k(loser);

    let winner_after = winner.rating + winner_k * (1.0 - expected_winner);
    let loser_after = loser.rating + loser_k * (0.0 - expected_loser);

    (
        PlayerRating {
            rating: winner_after,
            uncertainty: clamp_uncertainty(winner.uncertainty * UNCERTAINTY_DECAY),
            matches_played: winner.matches_played.saturating_add(1),
        },
        PlayerRating {
            rating: loser_after,
            uncertainty: clamp_uncertainty(loser.uncertainty * UNCERTAINTY_DECAY),
            matches_played: loser.matches_played.saturating_add(1),
        },
    )
}

/// Effective K-factor for a player, scaled up while provisional.
fn effective_k(player: &PlayerRating) -> f64 {
    if player.is_provisional() {
        K_FACTOR * PROVISIONAL_K_MULTIPLIER
    } else {
        K_FACTOR
    }
}
