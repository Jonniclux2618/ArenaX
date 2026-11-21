use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::models::tournament::{Tournament, TournamentStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RosterLockBoundary {
    /// Roster is mutable until registration closes.
    RegistrationClose,
    /// Roster is mutable until the bracket is seeded / rounds assigned.
    RoundAssignment,
    /// Roster is mutable until the first match starts.
    MatchStart,
}

impl RosterLockBoundary {
    pub fn from_rules(rules: &TournamentRules) -> Self {
        rules.roster_lock_boundary
    }

    /// Returns true when the roster is immutable for the given tournament state.
    pub fn is_locked(&self, status: TournamentStatus) -> bool {
        match self {
            RosterLockBoundary::RegistrationClose => matches!(
                status,
                TournamentStatus::RegistrationClosed
                    | TournamentStatus::Seeded
                    | TournamentStatus::InProgress
                    | TournamentStatus::Completed
            ),
            RosterLockBoundary::RoundAssignment => matches!(
                status,
                TournamentStatus::Seeded
                    | TournamentStatus::InProgress
                    | TournamentStatus::Completed
            ),
            RosterLockBoundary::MatchStart => matches!(
                status,
                TournamentStatus::InProgress | TournamentStatus::Completed
            ),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TournamentRules {
    pub roster_lock_boundary: RosterLockBoundary,
}

impl Default for TournamentRules {
    fn default() -> Self {
        Self {
            roster_lock_boundary: RosterLockBoundary::MatchStart,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RosterSubstitution {
    pub id: Uuid,
    pub team_id: Uuid,
    pub match_id: Uuid,
    pub outgoing_participant_id: Uuid,
    pub incoming_participant_id: Uuid,
    pub reason: String,
    pub recorded_at: DateTime<Utc>,
    pub recorded_by: Uuid,
}

#[derive(Debug)]
pub enum RosterError {
    RosterLocked,
    MatchActiveOrFinalized,
    ParticipantIdentityChange,
    NotFound,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for RosterError {
    fn from(err: sqlx::Error) -> Self {
        RosterError::Db(err)
    }
}

pub struct TournamentService {
    pool: PgPool,
}

impl TournamentService {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Canonical boundary for roster mutations. All roster writes must go through here.
    pub async fn update_roster(
        &self,
        tournament_id: Uuid,
        team_id: Uuid,
        match_id: Option<Uuid>,
        participant_id: Uuid,
        actor_id: Uuid,
    ) -> Result<(), RosterError> {
        let mut tx = self.pool.begin().await?;

        let tournament = self.load_tournament_for_update(&mut tx, tournament_id).await?;
        let rules = TournamentRules::default();
        let boundary = RosterLockBoundary::from_rules(&rules);

        if boundary.is_locked(tournament.status) {
            return Err(RosterError::RosterLocked);
        }

        if let Some(match_id) = match_id {
            let match_status = self.load_match_status(&mut tx, match_id).await?;
            if matches!(match_status.as_str(), "active" | "finalized") {
                return Err(RosterError::MatchActiveOrFinalized);
            }
        }

        self.apply_roster_update(&mut tx, team_id, participant_id).await?;
        self.record_audit(&mut tx, tournament_id, team_id, match_id, actor_id)
            .await?;

        tx.commit().await?;
        Ok(())
    }

    /// Records an allowed substitution against a team and match. Rejects any
    /// substitution that would change a participant identity in an active or
    /// finalized match.
    pub async fn record_substitution(
        &self,
        tournament_id: Uuid,
        team_id: Uuid,
        match_id: Uuid,
        outgoing_participant_id: Uuid,
        incoming_participant_id: Uuid,
        reason: String,
        actor_id: Uuid,
    ) -> Result<RosterSubstitution, RosterError> {
        let mut tx = self.pool.begin().await?;

        let tournament = self.load_tournament_for_update(&mut tx, tournament_id).await?;
        let rules = TournamentRules::default();
        let boundary = RosterLockBoundary::from_rules(&rules);

        if boundary.is_locked(tournament.status) {
            return Err(RosterError::RosterLocked);
        }

        let match_status = self.load_match_status(&mut tx, match_id).await?;
        if matches!(match_status.as_str(), "active" | "finalized") {
            return Err(RosterError::MatchActiveOrFinalized);
        }

        let substitution = RosterSubstitution {
            id: Uuid::new_v4(),
            team_id,
            match_id,
            outgoing_participant_id,
            incoming_participant_id,
            reason,
            recorded_at: Utc::now(),
            recorded_by: actor_id,
        };

        sqlx::query(
            r#"
            INSERT INTO roster_substitutions
                (id, tournament_id, team_id, match_id, outgoing_participant_id,
                 incoming_participant_id, reason, recorded_at, recorded_by)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            "#,
        )
        .bind(substitution.id)
        .bind(tournament_id)
        .bind(substitution.team_id)
        .bind(substitution.match_id)
        .bind(substitution.outgoing_participant_id)
        .bind(substitution.incoming_participant_id)
        .bind(&substitution.reason)
        .bind(substitution.recorded_at)
        .bind(substitution.recorded_by)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(substitution)
    }

    async fn load_tournament_for_update(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        tournament_id: Uuid,
    ) -> Result<Tournament, RosterError> {
        let tournament = sqlx::query_as::<_, Tournament>(
            "SELECT * FROM tournaments WHERE id = $1 FOR UPDATE",
        )
        .bind(tournament_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(RosterError::NotFound)?;
        Ok(tournament)
    }

    async fn load_match_status(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        match_id: Uuid,
    ) -> Result<String, RosterError> {
        let status = sqlx::query_scalar::<_, String>(
            "SELECT status FROM matches WHERE id = $1 FOR UPDATE",
        )
        .bind(match_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(RosterError::NotFound)?;
        Ok(status)
    }

    async fn apply_roster_update(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        team_id: Uuid,
        participant_id: Uuid,
    ) -> Result<(), RosterError> {
        sqlx::query(
            "UPDATE team_rosters SET participant_id = $1 WHERE team_id = $2",
        )
        .bind(participant_id)
        .bind(team_id)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    async fn record_audit(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        tournament_id: Uuid,
        team_id: Uuid,
        match_id: Option<Uuid>,
        actor_id: Uuid,
    ) -> Result<(), RosterError> {
        sqlx::query(
            r#"
            INSERT INTO roster_audit_log
                (id, tournament_id, team_id, match_id, actor_id, recorded_at)
            VALUES ($1, $2, $3, $4, $5, $6)
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(tournament_id)
        .bind(team_id)
        .bind(match_id)
        .bind(actor_id)
        .bind(Utc::now())
        .execute(&mut **tx)
        .await?;
        Ok(())
    }
}
