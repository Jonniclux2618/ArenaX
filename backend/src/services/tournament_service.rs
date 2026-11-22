use crate::models::tournament::{Tournament, TournamentConfig, TournamentStatus};
use crate::services::bracket_service::{self, BracketShape};
use crate::services::fee_service;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug)]
pub enum TournamentError {
    NotFound,
    InvalidConfig(Vec<ConfigIssue>),
    Conflict(String),
    Internal(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfigIssue {
    pub field: String,
    pub message: String,
}

/// Read-only result of validating a proposed tournament configuration.
/// Mirrors exactly what the create path would compute, without persisting anything.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TournamentConfigPreview {
    pub valid: bool,
    pub issues: Vec<ConfigIssue>,
    pub bracket: Option<BracketShape>,
    pub fee_cents: Option<i64>,
    pub capacity: Option<i64>,
}

/// Lifecycle state of a tournament result dispute. Transitions are strictly
/// forward-only; a closed dispute unlocks only the next valid transition.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum DisputeState {
    /// Opened, awaiting evidence within the deadline.
    Open,
    /// Evidence deadline passed; escalated to a reviewer.
    Escalated,
    /// Reviewer has resolved the dispute with a recorded reason.
    Resolved,
    /// Dispute closed; the underlying result may advance.
    Closed,
}

/// Versioned deadline and escalation policy for result disputes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisputePolicy {
    /// Bumped whenever deadline/escalation semantics change.
    pub version: u32,
    /// Window after opening during which evidence may be submitted.
    pub evidence_window: Duration,
    /// Window after the evidence deadline before auto-escalation.
    pub escalation_grace: Duration,
}

impl Default for DisputePolicy {
    fn default() -> Self {
        Self {
            version: 1,
            evidence_window: Duration::hours(48),
            escalation_grace: Duration::hours(24),
        }
    }
}

/// A dispute record capturing opening time, evidence deadline, reviewer
/// assignment, and resolution reason.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dispute {
    pub id: Uuid,
    pub tournament_id: Uuid,
    pub policy_version: u32,
    pub state: DisputeState,
    pub opened_at: DateTime<Utc>,
    pub evidence_deadline: DateTime<Utc>,
    pub reviewer_id: Option<Uuid>,
    pub resolution_reason: Option<String>,
    pub resolved_at: Option<DateTime<Utc>>,
}

impl Dispute {
    /// Open a new dispute, stamping the evidence deadline from the policy.
    pub fn open(tournament_id: Uuid, policy: &DisputePolicy, now: DateTime<Utc>) -> Self {
        Self {
            id: Uuid::new_v4(),
            tournament_id,
            policy_version: policy.version,
            state: DisputeState::Open,
            opened_at: now,
            evidence_deadline: now + policy.evidence_window,
            reviewer_id: None,
            resolution_reason: None,
            resolved_at: None,
        }
    }

    /// Whether evidence submitted at `at` is still within the deadline.
    pub fn accepts_evidence_at(&self, at: DateTime<Utc>) -> bool {
        self.state == DisputeState::Open && at <= self.evidence_deadline
    }

    /// Escalate a dispute whose evidence deadline (plus grace) has passed.
    /// Escalation never resolves or accepts the claim; it only assigns review.
    pub fn escalate(
        &mut self,
        policy: &DisputePolicy,
        reviewer_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<(), TournamentError> {
        if self.state != DisputeState::Open {
            return Err(TournamentError::Conflict(
                "only open disputes can be escalated".into(),
            ));
        }
        if now < self.evidence_deadline + policy.escalation_grace {
            return Err(TournamentError::Conflict(
                "escalation grace period has not elapsed".into(),
            ));
        }
        self.state = DisputeState::Escalated;
        self.reviewer_id = Some(reviewer_id);
        Ok(())
    }

    /// Reassign the reviewer without changing lifecycle state.
    pub fn reassign_reviewer(&mut self, reviewer_id: Uuid) -> Result<(), TournamentError> {
        if self.state != DisputeState::Escalated {
            return Err(TournamentError::Conflict(
                "reviewer can only be reassigned while escalated".into(),
            ));
        }
        self.reviewer_id = Some(reviewer_id);
        Ok(())
    }

    /// Resolve an escalated dispute with a mandatory reason. Repeated
    /// resolution requests are rejected so the record stays immutable.
    pub fn resolve(
        &mut self,
        reason: &str,
        now: DateTime<Utc>,
    ) -> Result<(), TournamentError> {
        if self.state != DisputeState::Escalated {
            return Err(TournamentError::Conflict(
                "only escalated disputes can be resolved".into(),
            ));
        }
        if reason.trim().is_empty() {
            return Err(TournamentError::Conflict(
                "resolution reason is required".into(),
            ));
        }
        self.state = DisputeState::Resolved;
        self.resolution_reason = Some(reason.to_string());
        self.resolved_at = Some(now);
        Ok(())
    }

    /// Close a resolved dispute, unlocking the next lifecycle transition.
    pub fn close(&mut self) -> Result<(), TournamentError> {
        if self.state != DisputeState::Resolved {
            return Err(TournamentError::Conflict(
                "only resolved disputes can be closed".into(),
            ));
        }
        self.state = DisputeState::Closed;
        Ok(())
    }
}

pub struct TournamentService {
    pool: PgPool,
}

impl TournamentService {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Shared validation used by both preview and create so they cannot diverge.
    fn validate_config(config: &TournamentConfig) -> Vec<ConfigIssue> {
        let mut issues = Vec::new();

        if config.max_participants <= 0 {
            issues.push(ConfigIssue {
                field: "max_participants".into(),
                message: "must be greater than zero".into(),
            });
        }

        if config.min_participants <= 0 {
            issues.push(ConfigIssue {
                field: "min_participants".into(),
                message: "must be greater than zero".into(),
            });
        }

        if config.min_participants > config.max_participants {
            issues.push(ConfigIssue {
                field: "min_participants".into(),
                message: "cannot exceed max_participants".into(),
            });
        }

        if config.entry_fee_cents < 0 {
            issues.push(ConfigIssue {
                field: "entry_fee_cents".into(),
                message: "cannot be negative".into(),
            });
        }

        if config.starts_at >= config.ends_at {
            issues.push(ConfigIssue {
                field: "starts_at".into(),
                message: "must be before ends_at".into(),
            });
        }

        issues
    }

    /// Dry-run preview for organizers. Performs no reservation, payment,
    /// notification, or persistent mutation.
    pub async fn preview_config(
        &self,
        config: &TournamentConfig,
    ) -> Result<TournamentConfigPreview, TournamentError> {
        let issues = Self::validate_config(config);

        if !issues.is_empty() {
            return Ok(TournamentConfigPreview {
                valid: false,
                issues,
                bracket: None,
                fee_cents: None,
                capacity: None,
            });
        }

        let bracket = bracket_service::compute_shape(config.max_participants);
        let fee_cents = fee_service::calculate_entry_fee(config);

        Ok(TournamentConfigPreview {
            valid: true,
            issues: Vec::new(),
            bracket: Some(bracket),
            fee_cents: Some(fee_cents),
            capacity: Some(config.max_participants),
        })
    }

    pub async fn create_tournament(
        &self,
        organizer_id: Uuid,
        config: &TournamentConfig,
    ) -> Result<Tournament, TournamentError> {
        let issues = Self::validate_config(config);
        if !issues.is_empty() {
            return Err(TournamentError::InvalidConfig(issues));
        }

        let bracket = bracket_service::compute_shape(config.max_participants);
        let fee_cents = fee_service::calculate_entry_fee(config);

        let now: DateTime<Utc> = Utc::now();
        let tournament = sqlx::query_as::<_, Tournament>(
            r#"
            INSERT INTO tournaments
                (id, organizer_id, status, max_participants, min_participants,
                 entry_fee_cents, bracket_size, starts_at, ends_at, created_at, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10)
            RETURNING *
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(organizer_id)
        .bind(TournamentStatus::Draft)
        .bind(config.max_participants)
        .bind(config.min_participants)
        .bind(fee_cents)
        .bind(bracket.size)
        .bind(config.starts_at)
        .bind(config.ends_at)
        .bind(now)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| TournamentError::Internal(e.to_string()))?;

        Ok(tournament)
    }
}
