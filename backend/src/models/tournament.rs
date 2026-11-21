#![allow(dead_code)]

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::FromRow;
use uuid::Uuid;
use validator::Validate;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Tournament {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub game: String, // Changed from game_type for consistency
    pub max_participants: i32,
    pub entry_fee: i64, // Changed from Decimal for database compatibility
    pub entry_fee_currency: String,
    pub prize_pool: i64, // Changed from Decimal
    pub prize_pool_currency: String,
    pub status: TournamentStatus,
    pub start_time: DateTime<Utc>,
    pub end_time: Option<DateTime<Utc>>,
    pub registration_deadline: DateTime<Utc>,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub bracket_type: BracketType,
    pub rules: Option<String>,
    pub min_skill_level: Option<i32>, // For skill-based matchmaking
    pub max_skill_level: Option<i32>,
    pub cleaned_up_at: Option<DateTime<Utc>>,
}

/// Immutable snapshot of the tournament rules a participant accepted at
/// registration. Lifecycle transitions and settlement MUST read from this
/// snapshot rather than the mutable `Tournament` row so that later edits to
/// tournament settings cannot retroactively change participant obligations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TournamentRulesSnapshot {
    /// Bumped whenever the snapshot schema or hashing changes.
    pub version: u32,
    /// Stable content hash over the canonical snapshot payload.
    pub hash: String,
    pub format: SnapshotFormatTerms,
    pub scoring: SnapshotScoringTerms,
    pub eligibility: SnapshotEligibilityTerms,
    pub timing: SnapshotTimingTerms,
    pub fee: SnapshotFeeTerms,
    pub payout: SnapshotPayoutTerms,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotFormatTerms {
    pub game: String,
    pub bracket_type: String,
    pub max_participants: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotScoringTerms {
    pub rules: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotEligibilityTerms {
    pub min_skill_level: Option<i32>,
    pub max_skill_level: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotTimingTerms {
    pub start_time: DateTime<Utc>,
    pub end_time: Option<DateTime<Utc>>,
    pub registration_deadline: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotFeeTerms {
    pub entry_fee: i64,
    pub entry_fee_currency: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotPayoutTerms {
    pub prize_pool: i64,
    pub prize_pool_currency: String,
}

/// Current snapshot schema version.
pub const RULES_SNAPSHOT_VERSION: u32 = 1;

impl TournamentRulesSnapshot {
    /// Build a snapshot from the current tournament row, computing the stable
    /// content hash over the canonical payload.
    pub fn capture(tournament: &Tournament) -> Self {
        let mut snapshot = Self {
            version: RULES_SNAPSHOT_VERSION,
            hash: String::new(),
            format: SnapshotFormatTerms {
                game: tournament.game.clone(),
                bracket_type: tournament.bracket_type.to_string(),
                max_participants: tournament.max_participants,
            },
            scoring: SnapshotScoringTerms {
                rules: tournament.rules.clone(),
            },
            eligibility: SnapshotEligibilityTerms {
                min_skill_level: tournament.min_skill_level,
                max_skill_level: tournament.max_skill_level,
            },
            timing: SnapshotTimingTerms {
                start_time: tournament.start_time,
                end_time: tournament.end_time,
                registration_deadline: tournament.registration_deadline,
            },
            fee: SnapshotFeeTerms {
                entry_fee: tournament.entry_fee,
                entry_fee_currency: tournament.entry_fee_currency.clone(),
            },
            payout: SnapshotPayoutTerms {
                prize_pool: tournament.prize_pool,
                prize_pool_currency: tournament.prize_pool_currency.clone(),
            },
        };
        snapshot.hash = snapshot.compute_hash();
        snapshot
    }

    /// Deterministic hash over the canonical snapshot payload (excluding the
    /// hash field itself). Used to detect drift and to key re-consent flows.
    pub fn compute_hash(&self) -> String {
        let canonical = serde_json::json!({
            "version": self.version,
            "format": self.format,
            "scoring": self.scoring,
            "eligibility": self.eligibility,
            "timing": self.timing,
            "fee": self.fee,
            "payout": self.payout,
        });
        let bytes = serde_json::to_vec(&canonical).unwrap_or_default();
        let digest = Sha256::digest(&bytes);
        format!("{:x}", digest)
    }

    /// Verify the stored hash still matches the payload (tamper/drift check).
    pub fn verify(&self) -> bool {
        self.hash == self.compute_hash()
    }

    /// True when the live tournament settings differ from the accepted
    /// snapshot, meaning a versioned re-consent flow is required.
    pub fn differs_from(&self, tournament: &Tournament) -> bool {
        let current = Self::capture(tournament);
        current.compute_hash() != self.compute_hash()
    }
}

/// Read-only dry-run preview of a proposed tournament configuration.
///
/// Produced by [`TournamentConfigPreview::preview`], which reuses the same
/// validation rules and bracket/fee calculation as the server-side create
/// path. A preview performs no reservation, payment, notification, or
/// persistent mutation — it only reports what the create path would do.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TournamentConfigPreview {
    /// True when the proposed configuration would pass create-path validation.
    pub valid: bool,
    /// Human-readable validation errors (empty when `valid`).
    pub errors: Vec<String>,
    /// Bracket shape the create path would generate (None when invalid).
    pub bracket: Option<BracketPreview>,
    /// Fee/payout calculation the create path would apply (None when invalid).
    pub fees: Option<FeePreview>,
    /// Rules snapshot the create path would capture for this configuration.
    pub rules_snapshot: Option<TournamentRulesSnapshot>,
}

/// Bracket shape implied by a configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BracketPreview {
    pub bracket_type: String,
    /// Number of participant slots the bracket is sized for.
    pub capacity: i32,
    /// Number of rounds in the bracket.
    pub rounds: u32,
    /// Number of byes required to fill the bracket to a power of two.
    pub byes: u32,
}

/// Fee and payout implications of a configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FeePreview {
    pub entry_fee: i64,
    pub entry_fee_currency: String,
    /// Total entry fees collected at full capacity.
    pub gross_entry_fees: i64,
    pub prize_pool: i64,
    pub prize_pool_currency: String,
}

impl TournamentConfigPreview {
    /// Compute a dry-run preview for a proposed configuration.
    ///
    /// This is the canonical preview entry point: it applies the same
    /// validation rules and bracket/fee calculation as the server-side create
    /// path, and performs no reservation, payment, notification, or persistent
    /// mutation.
    pub fn preview(tournament: &Tournament) -> Self {
        let errors = Self::validate_config(tournament);
        if !errors.is_empty() {
            return Self {
                valid: false,
                errors,
                bracket: None,
                fees: None,
                rules_snapshot: None,
            };
        }

        Self {
            valid: true,
            errors: Vec::new(),
            bracket: Some(Self::compute_bracket(tournament)),
            fees: Some(Self::compute_fees(tournament)),
            rules_snapshot: Some(TournamentRulesSnapshot::capture(tournament)),
        }
    }

    /// Validation rules shared with the server-side create path.
    fn validate_config(tournament: &Tournament) -> Vec<String> {
        let mut errors = Vec::new();

        if tournament.name.trim().is_empty() {
            errors.push("name must not be empty".to_string());
        }
        if tournament.game.trim().is_empty() {
            errors.push("game must not be empty".to_string());
        }
        if tournament.max_participants < 2 {
            errors.push("max_participants must be at least 2".to_string());
        }
        if tournament.entry_fee < 0 {
            errors.push("entry_fee must not be negative".to_string());
        }
        if tournament.prize_pool < 0 {
            errors.push("prize_pool must not be negative".to_string());
        }
        if tournament.entry_fee_currency.trim().is_empty() {
            errors.push("entry_fee_currency must not be empty".to_string());
        }
        if tournament.prize_pool_currency.trim().is_empty() {
            errors.push("prize_pool_currency must not be empty".to_string());
        }
        if tournament.registration_deadline >= tournament.start_time {
            errors.push("registration_deadline must be before start_time".to_string());
        }
        if let Some(end_time) = tournament.end_time {
            if end_time <= tournament.start_time {
                errors.push("end_time must be after start_time".to_string());
            }
        }
        if let (Some(min), Some(max)) =
            (tournament.min_skill_level, tournament.max_skill_level)
        {
            if min > max {
                errors.push("min_skill_level must not exceed max_skill_level".to_string());
            }
        }

        errors
    }

    /// Bracket shape calculation shared with the server-side create path.
    fn compute_bracket(tournament: &Tournament) -> BracketPreview {
        let capacity = tournament.max_participants;
        let (rounds, byes) = match tournament.bracket_type {
            BracketType::SingleElimination | BracketType::DoubleElimination => {
                let size = (capacity as u32).next_power_of_two();
                let rounds = size.trailing_zeros();
                let byes = size - capacity as u32;
                (rounds, byes)
            }
            BracketType::RoundRobin => {
                let rounds = if capacity > 1 { capacity as u32 - 1 } else { 0 };
                (rounds, 0)
            }
            BracketType::Swiss => {
                let rounds = (capacity as f64).log2().ceil() as u32;
                (rounds, 0)
            }
        };

        BracketPreview {
            bracket_type: tournament.bracket_type.to_string(),
            capacity,
            rounds,
            byes,
        }
    }

    /// Fee/payout calculation shared with the server-side create path.
    fn compute_fees(tournament: &Tournament) -> FeePreview {
        let gross_entry_fees = tournament.entry_fee * tournament.max_participants as i64;
        FeePreview {
            entry_fee: tournament.entry_fee,
            entry_fee_currency: tournament.entry_fee_currency.clone(),
            gross_entry_fees,
            prize_pool: tournament.prize_pool,
            prize_pool_currency: tournament.prize_pool_currency.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TournamentType {
    SingleElimination,
    DoubleElimination,
    RoundRobin,
    Swiss,
}

impl std::fmt::Display for TournamentType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TournamentType::SingleElimination => write!(f, "single_elimination"),
            TournamentType::DoubleElimination => write!(f, "double_elimination"),
            TournamentType::RoundRobin => write!(f, "round_robin"),
            TournamentType::Swiss => write!(f, "swiss"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, sqlx::Type)]
#[repr(i32)]
pub enum TournamentStatus {
    Draft = 0,
    Upcoming = 1,
    RegistrationOpen = 2,
    RegistrationClosed = 3,
    InProgress = 4,
    Completed = 5,
    Cancelled = 6,
}

impl std::fmt::Display for TournamentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TournamentStatus::Draft => write!(f, "draft"),
            TournamentStatus::Upcoming => write!(f, "upcoming"),
            TournamentStatus::RegistrationOpen => write!(f, "registration_open"),
            TournamentStatus::RegistrationClosed => write!(f, "registration_closed"),
            TournamentStatus::InProgress => write!(f, "in_progress"),
            TournamentStatus::Completed => write!(f, "completed"),
            TournamentStatus::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl From<i32> for TournamentStatus {
    fn from(v: i32) -> Self {
        match v {
            0 => TournamentStatus::Draft,
            1 => TournamentStatus::Upcoming,
            2 => TournamentStatus::RegistrationOpen,
            3 => TournamentStatus::RegistrationClosed,
            4 => TournamentStatus::InProgress,
            5 => TournamentStatus::Completed,
            6 => TournamentStatus::Cancelled,
            _ => TournamentStatus::Draft,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TournamentVisibility {
    Public,
    Private,
    InviteOnly,
}

impl std::fmt::Display for TournamentVisibility {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
     

/* … truncated 4689 chars — edit only what you need near the top … */
