#![allow(dead_code)]

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
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
    /// IANA timezone identifier used for organizer-facing display.
    pub timezone: String,
    /// UTC offset (seconds) captured at scheduling time for display.
    pub display_utc_offset_seconds: i32,
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
            TournamentVisibility::Public => write!(f, "public"),
            TournamentVisibility::Private => write!(f, "private"),
            TournamentVisibility::InviteOnly => write!(f, "invite_only"),
        }
    }
}

/// Explicit, user-visible rule for resolving local times that are ambiguous
/// (fall-back) or nonexistent (spring-forward) during DST transitions.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum DstResolutionRule {
    /// Ambiguous local times resolve to the earlier instant; nonexistent local
    /// times shift forward by the DST gap.
    Earlier,
    /// Ambiguous local times resolve to the later instant; nonexistent local
    /// times shift forward by the DST gap.
    Later,
}

impl Default for DstResolutionRule {
    fn default() -> Self {
        DstResolutionRule::Earlier
    }
}

impl std::fmt::Display for DstResolutionRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DstResolutionRule::Earlier => write!(f, "earlier"),
            DstResolutionRule::Later => write!(f, "later"),
        }
    }
}

/// Policy governing whether and how a team roster may be changed after the
/// roster lock. Substitutions are always audited and never rewrite rounds that
/// have already been played.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SubstitutionPolicy {
    /// No roster changes are permitted after lock.
    Locked,
    /// Substitutions are allowed only before the first round is played.
    BeforeFirstRound,
    /// Substitutions are allowed at any time, subject to roster size limits.
    Always,
}

impl Default for SubstitutionPolicy {
    fn default() -> Self {
        SubstitutionPolicy::BeforeFirstRound
    }
}

impl std::fmt::Display for SubstitutionPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SubstitutionPolicy::Locked => write!(f, "locked"),
            SubstitutionPolicy::BeforeFirstRound => write!(f, "before_first_round"),
            SubstitutionPolicy::Always => write!(f, "always"),
        }
    }
}

/// Team-level registration request. Captures the eligible roster snapshot,
/// captain, substitutions policy, and rules version at registration time.
#[derive(Debug, Clone, Serialize, Deserialize, Validate)]
pub struct TeamRegistrationRequest {
    pub tournament_id: Uuid,
    /// Captain who owns the registration and is responsible for the roster.
    pub captain_id: Uuid,
    /// Eligible roster snapshot (captain included).
    #[validate(length(min = 1, max = 64))]
    pub roster: Vec<Uuid>,
    #[serde(default)]
    pub substitution_policy: SubstitutionPolicy,
    /// Rules version the team is registering under.
    #[validate(length(min = 1, max = 64))]
    pub rules_version: String,
}

impl TeamRegistrationRequest {
    /// Validate the roster against the tournament's min/max team size and
    /// ensure the captain is part of the roster with no duplicate members.
    pub fn validate_roster(&self, min_team_size: i32, max_team_size: i32) -> Result<(), String> {
        if !self.roster.contains(&self.captain_id) {
            return Err("captain must be part of the roster".to_string());
        }
        let mut seen = std::collections::HashSet::new();
        for member in &self.roster {
            if !seen.insert(*member) {
                return Err(format!("duplicate roster member: {member}"));
            }
        }
        let size = self.roster.len() as i32;
        if size < min_team_size || size > max_team_size {
            return Err(format!(
                "roster size {size} outside allowed range {min_team_size}..={max_team_size}"
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Validate)]
pub struct CreateTournamentRequest {
    #[validate(length(min = 3, max = 255))]
    pub name: String,
    #[validate(length(max = 1000))]
    pub description: Option<String>,
    #[validate(length(min = 1, max = 50))]
    pub game: String,
    pub bracket_type: BracketType,
    pub entry_fee: i64,
    pub entry_fee_currency: String,
    #[validate(range(min = 2, max = 1000))]
    pub max_participants: i32,
    pub start_time: DateTime<Utc>,
    pub registration_deadline: DateTime<Utc>,
    pub rules: Option<String>,
    pub min_skill_level: Option<i32>,
    pub max_skill_level: Option<i32>,
    /// Required IANA timezone identifier (e.g. `America/New_York`).
    pub timezone: String,
    /// Rule applied when the local time is ambiguous or nonexistent.
    #[serde(default)]
    pub dst_resolution: DstResolutionRule,
}

impl CreateTournamentRequest {
    /// Validate the IANA timezone identifier, returning the parsed `Tz`.
    pub fn parse_timezone(&self) -> Result<Tz, String> {
        self.timezone
            .parse::<Tz>()
            .map_err(|_| format!("invalid IANA timezone: {}", self.timezone))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Validate)]
pub struct UpdateTournamentRequest {
    #[validate(length(min = 3, max = 255))]
    pub name: Option<String>,
    #[validate(length(max = 1000))]
    pub description: Option<String>,
    pub tournament_type: Option<TournamentType>,
    pub entry_fee: Option<Decimal>,
    #[validate(range(min = 2, max = 1000))]
    pub max_participants: Option<i32>,
    pub visibility: Option<TournamentVisibility>,
    pub registration_start: Option<DateTime<Utc>>,
    pub registration_end: Option<DateTime<Utc>>,
    pub start_time: Option<DateTime<Utc>>,
    pub end_time: Option<DateTime<Utc>>,
    pub rules: Option<serde_json::Value>,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TournamentResponse {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub game: String,
    pub max_participants: i32,
    pub current_participants: i32,
    pub entry_fee: i64,
    pub entry_fee_currency: String,
    pub prize_pool: i64,
    pub prize_pool_currency: String,
    pub status: TournamentStatus,
    pub start_time: DateTime<Utc>,
    pub end_time: Option<DateTime<Utc>>,
    pub registration_deadline: DateTime<Utc>,
    pub bracket_type: BracketType,
    pub can_join: bool,
    pub is_participant: bool,
    pub participant_status: Option<ParticipantStatus>,
    /// IANA timezone used for organizer-facing display.
    pub timezone: String,
    /// UTC offset (seconds) captured at scheduling time for display.
    pub display_utc_offset_seconds: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct TournamentParticipant {
    pub id: Uuid,
    pub tournament_id: Uuid,
    pub user_id: Uuid,
    pub registered_at: DateTime<Utc>,
    pub entry_fee_paid: bool,
    pub status: ParticipantStatus,
    pub seed_number: Option<i32>,
    pub current_round: Option<i32>,
    pub eliminated_at: Option<DateTime<Utc>>,
    pub final_rank: Option<i32>,
    pub prize_amount: Option<i64>,
    pub prize_currency: Op

/* … truncated 1930 chars — edit only what you need near the top … */
