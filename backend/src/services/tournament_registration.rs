use chrono::{DateTime, Duration, Utc};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::models::tournament::{RegistrationStatus, TournamentRegistration};

/// Bounded window a promoted player has to accept their offered spot.
const PROMOTION_ACCEPTANCE_WINDOW_MINUTES: i64 = 30;

#[derive(Debug)]
pub enum RegistrationError {
    TournamentNotFound,
    TournamentFull,
    AlreadyRegistered,
    NotEligible(String),
    WaitlistEntryNotFound,
    OfferExpired,
    OfferNotPending,
    Database(sqlx::Error),
}

impl From<sqlx::Error> for RegistrationError {
    fn from(err: sqlx::Error) -> Self {
        RegistrationError::Database(err)
    }
}

pub struct TournamentRegistrationService {
    pool: PgPool,
}

impl TournamentRegistrationService {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Register a player. If capacity is available the player is confirmed and a
    /// payment reservation is created atomically; otherwise they are appended to
    /// the waitlist with a stable FIFO ordering.
    pub async fn register(
        &self,
        tournament_id: Uuid,
        player_id: Uuid,
    ) -> Result<TournamentRegistration, RegistrationError> {
        let mut tx = self.pool.begin().await?;

        // Lock the tournament row so concurrent registrations serialize on capacity.
        let capacity: Option<i32> = sqlx::query_scalar(
            "SELECT capacity FROM tournaments WHERE id = $1 FOR UPDATE",
        )
        .bind(tournament_id)
        .fetch_optional(&mut *tx)
        .await?;

        let capacity = capacity.ok_or(RegistrationError::TournamentNotFound)?;

        let existing: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM tournament_registrations \
             WHERE tournament_id = $1 AND player_id = $2 \
             AND status <> 'withdrawn' LIMIT 1",
        )
        .bind(tournament_id)
        .bind(player_id)
        .fetch_optional(&mut *tx)
        .await?;

        if existing.is_some() {
            return Err(RegistrationError::AlreadyRegistered);
        }

        let confirmed_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM tournament_registrations \
             WHERE tournament_id = $1 AND status IN ('confirmed', 'offered')",
        )
        .bind(tournament_id)
        .fetch_one(&mut *tx)
        .await?;

        let now = Utc::now();
        let (status, waitlist_position) = if confirmed_count < capacity as i64 {
            (RegistrationStatus::Confirmed, None)
        } else {
            let next_position: i64 = sqlx::query_scalar(
                "SELECT COALESCE(MAX(waitlist_position), 0) + 1 \
                 FROM tournament_registrations WHERE tournament_id = $1",
            )
            .bind(tournament_id)
            .fetch_one(&mut *tx)
            .await?;
            (RegistrationStatus::Waitlisted, Some(next_position))
        };

        let registration = sqlx::query_as::<_, TournamentRegistration>(
            "INSERT INTO tournament_registrations \
             (id, tournament_id, player_id, status, waitlist_position, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $6) \
             RETURNING *",
        )
        .bind(Uuid::new_v4())
        .bind(tournament_id)
        .bind(player_id)
        .bind(status.as_str())
        .bind(waitlist_position)
        .bind(now)
        .fetch_one(&mut *tx)
        .await?;

        if matches!(registration.status, RegistrationStatus::Confirmed) {
            reserve_payment(&mut tx, tournament_id, player_id, now).await?;
        }

        tx.commit().await?;
        Ok(registration)
    }

    /// Withdraw a confirmed player and promote the next eligible waitlisted player.
    /// Capacity release and promotion happen in a single transaction so a spot is
    /// never double-filled.
    pub async fn withdraw(
        &self,
        tournament_id: Uuid,
        player_id: Uuid,
    ) -> Result<Option<TournamentRegistration>, RegistrationError> {
        let mut tx = self.pool.begin().await?;

        sqlx::query("SELECT id FROM tournaments WHERE id = $1 FOR UPDATE")
            .bind(tournament_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(RegistrationError::TournamentNotFound)?;

        let now = Utc::now();
        let affected = sqlx::query(
            "UPDATE tournament_registrations SET status = 'withdrawn', updated_at = $3 \
             WHERE tournament_id = $1 AND player_id = $2 AND status = 'confirmed'",
        )
        .bind(tournament_id)
        .bind(player_id)
        .bind(now)
        .execute(&mut *tx)
        .await?
        .rows_affected();

        if affected == 0 {
            tx.commit().await?;
            return Ok(None);
        }

        release_payment(&mut tx, tournament_id, player_id).await?;
        let promoted = promote_next(&mut tx, tournament_id, now).await?;

        tx.commit().await?;
        Ok(promoted)
    }

    /// Accept a pending promotion offer within its bounded window.
    pub async fn accept_offer(
        &self,
        tournament_id: Uuid,
        player_id: Uuid,
    ) -> Result<TournamentRegistration, RegistrationError> {
        let mut tx = self.pool.begin().await?;

        sqlx::query("SELECT id FROM tournaments WHERE id = $1 FOR UPDATE")
            .bind(tournament_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(RegistrationError::TournamentNotFound)?;

        let now = Utc::now();
        let row: Option<(Uuid, Option<DateTime<Utc>>)> = sqlx::query_as(
            "SELECT id, offer_expires_at FROM tournament_registrations \
             WHERE tournament_id = $1 AND player_id = $2 AND status = 'offered' FOR UPDATE",
        )
        .bind(tournament_id)
        .bind(player_id)
        .fetch_optional(&mut *tx)
        .await?;

        let (registration_id, expires_at) = row.ok_or(RegistrationError::OfferNotPending)?;
        if expires_at.map(|e| e <= now).unwrap_or(true) {
            return Err(RegistrationError::OfferExpired);
        }

        let registration = sqlx::query_as::<_, TournamentRegistration>(
            "UPDATE tournament_registrations \
             SET status = 'confirmed', offer_expires_at = NULL, updated_at = $2 \
             WHERE id = $1 RETURNING *",
        )
        .bind(registration_id)
        .bind(now)
        .fetch_one(&mut *tx)
        .await?;

        reserve_payment(&mut tx, tournament_id, player_id, now).await?;

        tx.commit().await?;
        Ok(registration)
    }

    /// Decline a pending offer; the spot advances to the next waitlisted player exactly once.
    pub async fn decline_offer(
        &self,
        tournament_id: Uuid,
        player_id: Uuid,
    ) -> Result<Option<TournamentRegistration>, RegistrationError> {
        let mut tx = self.pool.begin().await?;

        sqlx::query("SELECT id FROM tournaments WHERE id = $1 FOR UPDATE")
            .bind(tournament_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(RegistrationError::TournamentNotFound)?;

        let now = Utc::now();
        let affected = sqlx::query(
            "UPDATE tournament_registrations SET status = 'declined', offer_expires_at = NULL, updated_at = $3 \
             WHERE tournament_id = $1 AND player_id = $2 AND status = 'offered'",
        )
        .bind(tournament_id)
        .bind(player_id)
        .bind(now)
        .execute(&mut *tx)
        .await?
        .rows_affected();

        if affected == 0 {
            tx.commit().await?;
            return Ok(None);
        }

        let promoted = promote_next(&mut tx, tournament_id, now).await?;
        tx.commit().await?;
        Ok(promoted)
    }

    /// Expire stale offers and advance the waitlist. Safe to run repeatedly; each
    /// expired offer advances the queue exactly once.
    pub async fn expire_offers(&self, tournament_id: Uuid) -> Result<Vec<TournamentRegistration>, RegistrationError> {
        let mut tx = self.pool.begin().await?;

        sqlx::query("SELECT id FROM tournaments WHERE id = $1 FOR UPDATE")
            .bind(tournament_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(RegistrationError::TournamentNotFound)?;

        let now = Utc::now();
        let expired: Vec<Uuid> = sqlx::query_scalar(
            "UPDATE tournament_registrations SET status = 'expired', offer_expires_at = NULL, updated_at = $2 \
             WHERE tournament_id = $1 AND status = 'offered' AND offer_expires_at <= $2 \
             RETURNING id",
        )
        .bind(tournament_id)
        .bind(now)
        .fetch_all(&mut *tx)
        .await?;

        let mut promoted = Vec::new();
        for _ in expired {
            if let Some(reg) = promote_next(&mut tx, tournament_id, now).await? {
                promoted.push(reg);
            }
        }

        tx.commit().await?;
        Ok(promoted)
    }
}

/// Promote the next eligible waitlisted player in stable FIFO order. Eligibility is
/// rechecked at promotion time; ineligible entries are skipped. Returns the offered
/// registration, if any.
async fn promote_next(
    tx: &mut Transaction<'_, Postgres>,
    tournament_id: Uuid,
    now: DateTime<Utc>,
) -> Result<Option<TournamentRegistration>, RegistrationError> {
    let capacity: i32 = sqlx::query_scalar("SELECT capacity FROM tournaments WHERE id = $1")
        .bind(tournament_id)
        .fetch_one(&mut **tx)
        .await?;

    let occupied: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM tournament_registrations \
         WHERE tournament_id = $1 AND status IN ('confirmed', 'offered')",
    )
    .bind(tournament_id)
    .fetch_one(&mut **tx)
    .await?;

    if occupied >= capacity as i64 {
        return Ok(None);
    }

    // Stable FIFO: order by enqueue position, then creation time as a tiebreaker.
    let candidates: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT id, player_id FROM tournament_registrations \
         WHERE tournament_id = $1 AND status = 'waitlisted' \
         ORDER BY waitlist_position ASC NULLS LAST, created_at ASC, id ASC \
         FOR UPDATE",
    )
    .bind(tournament_id)
    .fetch_all(&mut **tx)
    .await?;

    for (registration_id, player_id) in candidates {
        if !is_eligible(tx, tournament_id, player_id).await? {
            sqlx::query(
                "UPDATE tournament_registrations SET status = 'ineligible', updated_at = $2 WHERE id = $1",
            )
            .bind(registration_id)
            .bind(now)
            .execute(&mut **tx)
            .await?;
            continue;
        }

        let expires_at = now + Duration::minutes(PROMOTION_ACCEPTANCE_WINDOW_MINUTES);
        let registration = sqlx::query_as::<_, TournamentRegistration>(
            "UPDATE tournament_registrations \
             SET status = 'offered', offer_expires_at = $2, waitlist_position = NULL, updated_at = $3 \
             WHERE id = $1 RETURNING *",
        )
        .bind(registration_id)
        .bind(expires_at)
        .bind(now)
        .fetch_one(&mut **tx)
        .await?;

        return Ok(Some(registration));
    }

    Ok(None)
}

/// Recheck eligibility at promotion time (e.g. player not banned, still active).
async fn is_eligible(
    tx: &mut Transaction<'_, Postgres>,
    tournament_id: Uuid,
    player_id: Uuid,
) -> Result<bool, RegistrationError> {
    let eligible: Option<bool> = sqlx::query_scalar(
        "SELECT (p.is_active AND NOT p.is_banned) \
         FROM players p WHERE p.id = $1",
    )
    .bind(player_id)
    .fetch_optional(&mut **tx)
    .await?;

    let _ = tournament_id;
    Ok(eligible.unwrap_or(false))
}

async fn reserve_payment(
    tx: &mut Transaction<'_, Postgres>,
    tournament_id: Uuid,
    player_id: Uuid,
    now: DateTime<Utc>,
) -> Result<(), RegistrationError> {
    sqlx::query(
        "INSERT INTO payment_reservations (id, tournament_id, player_id, status, created_at, updated_at) \
         VALUES ($1, $2, $3, 'reserved', $4, $4) \
         ON CONFLICT (tournament_id, player_id) DO UPDATE \
         SET status = 'reserved', updated_at = $4",
    )
    .bind(Uuid::new_v4())
    .bind(tournament_id)
    .bind(player_id)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn release_payment(
    tx: &mut Transaction<'_, Postgres>,
    tournament_id: Uuid,
    player_id: Uuid,
) -> Result<(), RegistrationError> {
    sqlx::query(
        "UPDATE payment_reservations SET status = 'released', updated_at = NOW() \
         WHERE tournament_id = $1 AND player_id = $2 AND status = 'reserved'",
    )
    .bind(tournament_id)
    .bind(player_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}
