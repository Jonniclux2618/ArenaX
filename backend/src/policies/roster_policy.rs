//! Roster lock policy.
//!
//! Defines the tournament lifecycle boundary at which a team roster becomes
//! immutable, and the rules under which substitutions are still permitted.
//! This is the canonical policy consulted by the roster service boundary
//! before any roster mutation is applied.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Lifecycle phase of a tournament, ordered from earliest to latest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TournamentPhase {
    /// Registration is open; rosters are freely editable.
    Registration,
    /// Registration closed, rounds being assigned; rosters editable until lock.
    RoundAssignment,
    /// First match has started; rosters are locked.
    InProgress,
    /// Tournament completed; records are finalized and immutable.
    Finalized,
}

/// Per-tournament rules governing when the roster locks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RosterLockBoundary {
    /// Lock when registration closes (before round assignment).
    RegistrationClose,
    /// Lock when round assignment begins.
    RoundAssignmentStart,
    /// Lock when the first match starts (default).
    FirstMatchStart,
}

impl Default for RosterLockBoundary {
    fn default() -> Self {
        RosterLockBoundary::FirstMatchStart
    }
}

/// Tournament rules relevant to roster locking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TournamentRosterRules {
    pub lock_boundary: RosterLockBoundary,
    /// Whether substitutions are allowed after the lock boundary.
    pub allow_substitutions_after_lock: bool,
}

impl Default for TournamentRosterRules {
    fn default() -> Self {
        Self {
            lock_boundary: RosterLockBoundary::default(),
            allow_substitutions_after_lock: false,
        }
    }
}

/// Reason a roster mutation was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RosterLockError {
    /// The roster is locked and the mutation is not an allowed substitution.
    RosterLocked,
    /// The mutation would change a participant identity in an active or
    /// finalized match.
    ParticipantIdentityLocked { match_id: String },
    /// The tournament is finalized; only the audited correction flow may edit.
    TournamentFinalized,
    /// Substitutions are not permitted by the tournament rules.
    SubstitutionsNotAllowed,
}

impl std::fmt::Display for RosterLockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RosterLockError::RosterLocked => write!(f, "roster is locked"),
            RosterLockError::ParticipantIdentityLocked { match_id } => write!(
                f,
                "cannot change participant identity in active or finalized match {match_id}"
            ),
            RosterLockError::TournamentFinalized => {
                write!(f, "tournament is finalized; use the audited correction flow")
            }
            RosterLockError::SubstitutionsNotAllowed => {
                write!(f, "substitutions are not allowed by tournament rules")
            }
        }
    }
}

impl std::error::Error for RosterLockError {}

/// A single roster mutation request evaluated against the lock policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterMutation {
    /// Whether this mutation is a substitution (replacing a participant).
    pub is_substitution: bool,
    /// The participant identity being removed, if any.
    pub removed_participant: Option<String>,
    /// The participant identity being added, if any.
    pub added_participant: Option<String>,
    /// Matches in which the removed participant is currently active or finalized.
    pub active_or_finalized_match_ids: Vec<String>,
}

/// The roster lock policy.
#[derive(Debug, Clone, Copy, Default)]
pub struct RosterPolicy;

impl RosterPolicy {
    /// Returns whether the roster is locked at the given phase under the rules.
    pub fn is_locked(phase: TournamentPhase, rules: &TournamentRosterRules) -> bool {
        match rules.lock_boundary {
            RosterLockBoundary::RegistrationClose => phase >= TournamentPhase::RoundAssignment,
            RosterLockBoundary::RoundAssignmentStart => phase >= TournamentPhase::RoundAssignment,
            RosterLockBoundary::FirstMatchStart => phase >= TournamentPhase::InProgress,
        }
    }

    /// Evaluates a roster mutation against the lock boundary and rules.
    ///
    /// Returns `Ok(())` when the mutation is permitted, or the reason it is
    /// rejected. Finalized tournaments are always immutable here; corrections
    /// must go through the explicit audited correction flow.
    pub fn authorize_mutation(
        phase: TournamentPhase,
        rules: &TournamentRosterRules,
        mutation: &RosterMutation,
    ) -> Result<(), RosterLockError> {
        if phase == TournamentPhase::Finalized {
            return Err(RosterLockError::TournamentFinalized);
        }

        // A participant identity in an active or finalized match can never be
        // changed by a roster update, regardless of lock boundary.
        if let Some(match_id) = mutation.active_or_finalized_match_ids.first() {
            return Err(RosterLockError::ParticipantIdentityLocked {
                match_id: match_id.clone(),
            });
        }

        if !Self::is_locked(phase, rules) {
            return Ok(());
        }

        if mutation.is_substitution {
            if rules.allow_substitutions_after_lock {
                Ok(())
            } else {
                Err(RosterLockError::SubstitutionsNotAllowed)
            }
        } else {
            Err(RosterLockError::RosterLocked)
        }
    }
}

/// A recorded substitution, attributed to a team and match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubstitutionRecord {
    pub team_id: String,
    pub match_id: String,
    pub removed_participant: String,
    pub added_participant: String,
    pub recorded_at: DateTime<Utc>,
    pub authorized_by: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mutation(is_substitution: bool, matches: Vec<&str>) -> RosterMutation {
        RosterMutation {
            is_substitution,
            removed_participant: Some("p1".into()),
            added_participant: Some("p2".into()),
            active_or_finalized_match_ids: matches.into_iter().map(String::from).collect(),
        }
    }

    #[test]
    fn registration_allows_edits() {
        let rules = TournamentRosterRules::default();
        assert!(RosterPolicy::authorize_mutation(
            TournamentPhase::Registration,
            &rules,
            &mutation(false, vec![])
        )
        .is_ok());
    }

    #[test]
    fn first_match_start_locks_roster() {
        let rules = TournamentRosterRules::default();
        assert_eq!(
            RosterPolicy::authorize_mutation(
                TournamentPhase::InProgress,
                &rules,
                &mutation(false, vec![])
            ),
            Err(RosterLockError::RosterLocked)
        );
    }

    #[test]
    fn substitution_allowed_after_lock_when_rules_permit() {
        let rules = TournamentRosterRules {
            lock_boundary: RosterLockBoundary::FirstMatchStart,
            allow_substitutions_after_lock: true,
        };
        assert!(RosterPolicy::authorize_mutation(
            TournamentPhase::InProgress,
            &rules,
            &mutation(true, vec![])
        )
        .is_ok());
    }

    #[test]
    fn substitution_rejected_when_rules_disallow() {
        let rules = TournamentRosterRules::default();
        assert_eq!(
            RosterPolicy::authorize_mutation(
                TournamentPhase::InProgress,
                &rules,
                &mutation(true, vec![])
            ),
            Err(RosterLockError::SubstitutionsNotAllowed)
        );
    }

    #[test]
    fn active_match_identity_is_immutable() {
        let rules = TournamentRosterRules {
            lock_boundary: RosterLockBoundary::FirstMatchStart,
            allow_substitutions_after_lock: true,
        };
        assert_eq!(
            RosterPolicy::authorize_mutation(
                TournamentPhase::InProgress,
                &rules,
                &mutation(true, vec!["m1"])
            ),
            Err(RosterLockError::ParticipantIdentityLocked {
                match_id: "m1".into()
            })
        );
    }

    #[test]
    fn finalized_tournament_is_immutable() {
        let rules = TournamentRosterRules::default();
        assert_eq!(
            RosterPolicy::authorize_mutation(
                TournamentPhase::Finalized,
                &rules,
                &mutation(false, vec![])
            ),
            Err(RosterLockError::TournamentFinalized)
        );
    }
}
