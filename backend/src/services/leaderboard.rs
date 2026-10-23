//! Leaderboard and player-stat service.
//!
//! Source of truth: finalized match events (`FinalizedMatch`). Every rating
//! change is derived from these events only. The leaderboard is a pure
//! projection of the authoritative match history, so it can always be rebuilt.
//!
//! Guarantees:
//! - Idempotent incremental updates: a duplicate finalized-match event cannot
//!   apply rating changes twice (deduplicated by `match_id`).
//! - Reproducible rebuilds: replaying the authoritative history through the
//!   same versioned formula yields the same snapshot as incremental processing.
//! - Auditable corrections: every applied change records a before/after entry.

use std::collections::{BTreeMap, HashMap, HashSet};

/// Version of the rating formula. Bump when the formula changes so that
/// snapshots can be reproduced with the formula that produced them.
pub const RATING_FORMULA_VERSION: u32 = 1;

/// Elo K-factor used by formula version 1.
const K_FACTOR: f64 = 32.0;

/// A finalized match event. This is the authoritative source of truth for
/// rating changes. `match_id` uniquely identifies the event so duplicates can
/// be detected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalizedMatch {
    pub match_id: String,
    pub winner: String,
    pub loser: String,
    /// Monotonic sequence number assigned by the authoritative match store.
    /// Used for deterministic ordering when replaying history.
    pub sequence: u64,
}

/// A single auditable rating change.
#[derive(Debug, Clone, PartialEq)]
pub struct RatingChange {
    pub match_id: String,
    pub player: String,
    pub before: f64,
    pub after: f64,
}

/// A player's rating entry in the leaderboard.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerStat {
    pub player: String,
    pub rating: f64,
    pub wins: u32,
    pub losses: u32,
}

/// A reproducible ranking snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct LeaderboardSnapshot {
    pub formula_version: u32,
    pub players: Vec<PlayerStat>,
}

/// In-memory leaderboard state with idempotent incremental updates.
#[derive(Debug, Clone)]
pub struct Leaderboard {
    stats: BTreeMap<String, PlayerStat>,
    applied: HashSet<String>,
    audit_log: Vec<RatingChange>,
}

impl Default for Leaderboard {
    fn default() -> Self {
        Self::new()
    }
}

impl Leaderboard {
    pub fn new() -> Self {
        Self {
            stats: BTreeMap::new(),
            applied: HashSet::new(),
            audit_log: Vec::new(),
        }
    }

    /// Apply a finalized match event incrementally.
    ///
    /// Returns `false` if the event was already applied (duplicate), in which
    /// case no rating change occurs.
    pub fn apply(&mut self, event: &FinalizedMatch) -> bool {
        if self.applied.contains(&event.match_id) {
            return false;
        }

        let winner_before = self.rating_of(&event.winner);
        let loser_before = self.rating_of(&event.loser);

        let (winner_after, loser_after) =
            elo_update(winner_before, loser_before);

        self.set_rating(&event.winner, winner_after, true);
        self.set_rating(&event.loser, loser_after, false);

        self.audit_log.push(RatingChange {
            match_id: event.match_id.clone(),
            player: event.winner.clone(),
            before: winner_before,
            after: winner_after,
        });
        self.audit_log.push(RatingChange {
            match_id: event.match_id.clone(),
            player: event.loser.clone(),
            before: loser_before,
            after: loser_after,
        });

        self.applied.insert(event.match_id.clone());
        true
    }

    /// Rebuild the leaderboard from authoritative match history.
    ///
    /// History is sorted by `sequence` (then `match_id` for determinism) and
    /// replayed through the same formula, producing a snapshot identical to
    /// incremental processing of the same events.
    pub fn rebuild(history: &[FinalizedMatch]) -> Self {
        let mut ordered: Vec<&FinalizedMatch> = history.iter().collect();
        ordered.sort_by(|a, b| {
            a.sequence
                .cmp(&b.sequence)
                .then_with(|| a.match_id.cmp(&b.match_id))
        });

        let mut board = Self::new();
        for event in ordered {
            board.apply(event);
        }
        board
    }

    /// Produce a deterministic ranking snapshot.
    ///
    /// Tie-breaking: higher rating first, then more wins, then fewer losses,
    /// then player name ascending. This makes the ordering total and stable.
    pub fn snapshot(&self) -> LeaderboardSnapshot {
        let mut players: Vec<PlayerStat> = self.stats.values().cloned().collect();
        players.sort_by(|a, b| {
            b.rating
                .partial_cmp(&a.rating)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.wins.cmp(&a.wins))
                .then_with(|| a.losses.cmp(&b.losses))
                .then_with(|| a.player.cmp(&b.player))
        });
        LeaderboardSnapshot {
            formula_version: RATING_FORMULA_VERSION,
            players,
        }
    }

    /// Auditable before/after record of every applied rating change.
    pub fn audit_log(&self) -> &[RatingChange] {
        &self.audit_log
    }

    fn rating_of(&self, player: &str) -> f64 {
        self.stats
            .get(player)
            .map(|s| s.rating)
            .unwrap_or_else(default_rating)
    }

    fn set_rating(&mut self, player: &str, rating: f64, won: bool) {
        let entry = self
            .stats
            .entry(player.to_string())
            .or_insert_with(|| PlayerStat {
                player: player.to_string(),
                rating: default_rating(),
                wins: 0,
                losses: 0,
            });
        entry.rating = rating;
        if won {
            entry.wins += 1;
        } else {
            entry.losses += 1;
        }
    }
}

fn default_rating() -> f64 {
    1500.0
}

/// Versioned Elo update (formula version 1).
fn elo_update(winner: f64, loser: f64) -> (f64, f64) {
    let expected_winner = 1.0 / (1.0 + 10f64.powf((loser - winner) / 400.0));
    let expected_loser = 1.0 - expected_winner;
    let winner_after = winner + K_FACTOR * (1.0 - expected_winner);
    let loser_after = loser + K_FACTOR * (0.0 - expected_loser);
    (winner_after, loser_after)
}

/// Convenience: build a snapshot directly from authoritative history.
pub fn snapshot_from_history(history: &[FinalizedMatch]) -> LeaderboardSnapshot {
    Leaderboard::rebuild(history).snapshot()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(id: &str, seq: u64, winner: &str, loser: &str) -> FinalizedMatch {
        FinalizedMatch {
            match_id: id.to_string(),
            winner: winner.to_string(),
            loser: loser.to_string(),
            sequence: seq,
        }
    }

    #[test]
    fn duplicate_event_is_not_applied_twice() {
        let mut board = Leaderboard::new();
        let e = event("m1", 1, "alice", "bob");
        assert!(board.apply(&e));
        assert!(!board.apply(&e));
        assert_eq!(board.audit_log().len(), 2);
    }

    #[test]
    fn rebuild_matches_incremental() {
        let history = vec![
            event("m1", 1, "alice", "bob"),
            event("m2", 2, "carol", "alice"),
            event("m3", 3, "bob", "carol"),
        ];
        let mut incremental = Leaderboard::new();
        for e in &history {
            incremental.apply(e);
        }
        let rebuilt = Leaderboard::rebuild(&history);
        assert_eq!(incremental.snapshot(), rebuilt.snapshot());
    }

    #[test]
    fn rebuild_is_order_independent() {
        let history = vec![
            event("m2", 2, "carol", "alice"),
            event("m1", 1, "alice", "bob"),
        ];
        let mut shuffled = history.clone();
        shuffled.reverse();
        assert_eq!(
            Leaderboard::rebuild(&history).snapshot(),
            Leaderboard::rebuild(&shuffled).snapshot()
        );
    }
}
