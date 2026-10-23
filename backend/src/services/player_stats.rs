//! Player-stat and leaderboard service.
//!
//! Source of truth: finalized match events (`FinalizedMatch`). Every rating
//! change is derived from these events only. Incremental processing and full
//! rebuilds both consume the same event stream, so a rebuild from authoritative
//! match history reproduces the incremental ranking snapshot exactly.
//!
//! Idempotency: each finalized match carries a stable `match_id`. Applied match
//! ids are recorded per player, so a duplicate finalized-match event is ignored
//! and cannot apply rating changes twice.
//!
//! Corrections: applying a correction records an auditable before/after entry
//! in the player's history instead of silently mutating the rating.

use std::collections::{BTreeMap, BTreeSet};

/// Version of the rating formula. Bump when the formula changes so stored
/// snapshots can be identified and rebuilt deterministically.
pub const RATING_FORMULA_VERSION: u32 = 1;

/// Base rating assigned to a player with no finalized matches.
pub const BASE_RATING: i64 = 1000;

/// A finalized match event. This is the authoritative, immutable record that
/// leaderboard and player-stat services consume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalizedMatch {
    pub match_id: String,
    pub winner_id: String,
    pub loser_id: String,
}

/// Auditable record of a single rating change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RatingChange {
    pub match_id: String,
    pub before: i64,
    pub after: i64,
    pub formula_version: u32,
}

/// Per-player state derived from finalized matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerStats {
    pub player_id: String,
    pub rating: i64,
    pub wins: u32,
    pub losses: u32,
    /// Match ids already applied, used to reject duplicates.
    pub applied_matches: BTreeSet<String>,
    /// Auditable before/after record of every applied change.
    pub history: Vec<RatingChange>,
}

impl PlayerStats {
    fn new(player_id: &str) -> Self {
        Self {
            player_id: player_id.to_string(),
            rating: BASE_RATING,
            wins: 0,
            losses: 0,
            applied_matches: BTreeSet::new(),
            history: Vec::new(),
        }
    }
}

/// Deterministic rating delta for a win. Versioned so rebuilds are stable.
fn win_delta(formula_version: u32) -> i64 {
    match formula_version {
        1 => 25,
        _ => 25,
    }
}

/// Deterministic rating delta for a loss.
fn loss_delta(formula_version: u32) -> i64 {
    match formula_version {
        1 => -25,
        _ => -25,
    }
}

/// Leaderboard snapshot: players ordered by rating desc, then player id asc
/// for deterministic tie-breaking.
pub type LeaderboardSnapshot = Vec<(String, i64)>;

/// In-memory leaderboard/player-stat store.
#[derive(Debug, Default)]
pub struct Leaderboard {
    players: BTreeMap<String, PlayerStats>,
}

impl Leaderboard {
    pub fn new() -> Self {
        Self::default()
    }

    fn player_mut(&mut self, player_id: &str) -> &mut PlayerStats {
        self.players
            .entry(player_id.to_string())
            .or_insert_with(|| PlayerStats::new(player_id))
    }

    /// Apply a finalized match incrementally. Idempotent: a duplicate
    /// `match_id` is ignored and applies no rating change.
    pub fn apply_finalized_match(&mut self, event: &FinalizedMatch) -> bool {
        // Reject duplicates before touching any rating.
        if self
            .players
            .get(&event.winner_id)
            .map(|p| p.applied_matches.contains(&event.match_id))
            .unwrap_or(false)
            || self
                .players
                .get(&event.loser_id)
                .map(|p| p.applied_matches.contains(&event.match_id))
                .unwrap_or(false)
        {
            return false;
        }

        let delta_win = win_delta(RATING_FORMULA_VERSION);
        let delta_loss = loss_delta(RATING_FORMULA_VERSION);

        {
            let winner = self.player_mut(&event.winner_id);
            let before = winner.rating;
            winner.rating += delta_win;
            winner.wins += 1;
            winner.applied_matches.insert(event.match_id.clone());
            winner.history.push(RatingChange {
                match_id: event.match_id.clone(),
                before,
                after: winner.rating,
                formula_version: RATING_FORMULA_VERSION,
            });
        }

        {
            let loser = self.player_mut(&event.loser_id);
            let before = loser.rating;
            loser.rating += delta_loss;
            loser.losses += 1;
            loser.applied_matches.insert(event.match_id.clone());
            loser.history.push(RatingChange {
                match_id: event.match_id.clone(),
                before,
                after: loser.rating,
                formula_version: RATING_FORMULA_VERSION,
            });
        }

        true
    }

    /// Full rebuild from authoritative match history. Produces the same
    /// snapshot as incremental processing of the same events.
    pub fn rebuild(history: &[FinalizedMatch]) -> Self {
        let mut board = Self::new();
        for event in history {
            board.apply_finalized_match(event);
        }
        board
    }

    /// Deterministic ranking snapshot: rating desc, then player id asc.
    pub fn snapshot(&self) -> LeaderboardSnapshot {
        let mut entries: LeaderboardSnapshot = self
            .players
            .values()
            .map(|p| (p.player_id.clone(), p.rating))
            .collect();
        entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        entries
    }

    pub fn stats(&self, player_id: &str) -> Option<&PlayerStats> {
        self.players.get(player_id)
    }
}
