use chrono::{DateTime, Duration, Utc};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::AppError;
use crate::models::{Payment, PaymentStatus, Registration, RegistrationStatus};

/// How long a seat reservation holds capacity while payment is pending.
const RESERVATION_TTL_MINUTES: i64 = 15;

/// A unique reservation token binds one user/team, one tournament, and one expiry.
#[derive(Debug, Clone)]
pub struct SeatReservation {
    pub token: Uuid,
    pub tournament_id: Uuid,
    pub user_id: Uuid,
    pub team_id: Option<Uuid>,
    pub expires_at: DateTime<Utc>,
}

pub struct PaymentService {
    pool: PgPool,
}

impl PaymentService {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Atomically reserve a seat and create a bounded payment-pending registration.
    ///
    /// The seat count is checked and the reservation inserted inside a single
    /// transaction with a row lock on the tournament, so concurrent registrations
    /// at the last available seat cannot exceed capacity.
    pub async fn reserve_seat_and_start_payment(
        &self,
        tournament_id: Uuid,
        user_id: Uuid,
        team_id: Option<Uuid>,
    ) -> Result<(SeatReservation, Payment), AppError> {
        let mut tx = self.pool.begin().await?;

        // Lock the tournament row to serialize capacity checks.
        let capacity: i32 = sqlx::query_scalar(
            "SELECT capacity FROM tournaments WHERE id = $1 FOR UPDATE",
        )
        .bind(tournament_id)
        .fetch_one(&mut *tx)
        .await?;

        // Count seats held by confirmed registrations plus live (unexpired) reservations.
        let occupied: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*) FROM registrations
            WHERE tournament_id = $1
              AND (
                status = 'confirmed'
                OR (status = 'payment_pending' AND reservation_expires_at > now())
              )
            "#,
        )
        .bind(tournament_id)
        .fetch_one(&mut *tx)
        .await?;

        if occupied >= capacity as i64 {
            return Err(AppError::Conflict(
                "tournament capacity reached".to_string(),
            ));
        }

        let token = Uuid::new_v4();
        let expires_at = Utc::now() + Duration::minutes(RESERVATION_TTL_MINUTES);

        let registration = sqlx::query_as::<_, Registration>(
            r#"
            INSERT INTO registrations
                (id, tournament_id, user_id, team_id, status, reservation_token, reservation_expires_at)
            VALUES ($1, $2, $3, $4, 'payment_pending', $5, $6)
            RETURNING *
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(tournament_id)
        .bind(user_id)
        .bind(team_id)
        .bind(token)
        .bind(expires_at)
        .fetch_one(&mut *tx)
        .await?;

        let payment = sqlx::query_as::<_, Payment>(
            r#"
            INSERT INTO payments (id, registration_id, status, amount, created_at)
            VALUES ($1, $2, 'pending', $3, now())
            RETURNING *
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(registration.id)
        .bind(0i64)
        .fetch_one(&mut *tx)
        .await?;

        tx.commit().await?;

        Ok((
            SeatReservation {
                token,
                tournament_id,
                user_id,
                team_id,
                expires_at,
            },
            payment,
        ))
    }

    /// Reconcile a payment callback against its reservation.
    ///
    /// If the reservation is still live, the seat is confirmed. If it has expired,
    /// the seat is released and a deterministic refund is recorded instead of
    /// silently confirming an over-capacity seat.
    pub async fn reconcile_payment(
        &self,
        reservation_token: Uuid,
        payment_reference: &str,
    ) -> Result<RegistrationStatus, AppError> {
        let mut tx = self.pool.begin().await?;

        let registration = sqlx::query_as::<_, Registration>(
            r#"
            SELECT * FROM registrations
            WHERE reservation_token = $1
            FOR UPDATE
            "#,
        )
        .bind(reservation_token)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::NotFound("reservation not found".to_string()))?;

        // Idempotent replay: already reconciled, return the settled status.
        if registration.status == RegistrationStatus::Confirmed
            || registration.status == RegistrationStatus::Refunded
        {
            tx.commit().await?;
            return Ok(registration.status);
        }

        let now = Utc::now();
        let expired = registration
            .reservation_expires_at
            .map(|exp| exp <= now)
            .unwrap_or(true);

        let new_status = if expired {
            // Late success after expiry: deterministic refund path.
            sqlx::query(
                r#"
                UPDATE payments
                SET status = 'refunded', reference = $2, updated_at = now()
                WHERE registration_id = $1
                "#,
            )
            .bind(registration.id)
            .bind(payment_reference)
            .execute(&mut *tx)
            .await?;
            RegistrationStatus::Refunded
        } else {
            sqlx::query(
                r#"
                UPDATE payments
                SET status = 'succeeded', reference = $2, updated_at = now()
                WHERE registration_id = $1
                "#,
            )
            .bind(registration.id)
            .bind(payment_reference)
            .execute(&mut *tx)
            .await?;
            RegistrationStatus::Confirmed
        };

        sqlx::query(
            r#"
            UPDATE registrations
            SET status = $2, reservation_expires_at = NULL, updated_at = now()
            WHERE id = $1
            "#,
        )
        .bind(registration.id)
        .bind(new_status.as_str())
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(new_status)
    }

    /// Release expired reservations so their seats become available again.
    pub async fn release_expired_reservations(&self) -> Result<u64, AppError> {
        let mut tx: Transaction<'_, Postgres> = self.pool.begin().await?;

        let released = sqlx::query(
            r#"
            UPDATE registrations
            SET status = 'expired', updated_at = now()
            WHERE status = 'payment_pending'
              AND reservation_expires_at IS NOT NULL
              AND reservation_expires_at <= now()
            "#,
        )
        .execute(&mut *tx)
        .await?
        .rows_affected();

        tx.commit().await?;
        Ok(released)
    }
}
