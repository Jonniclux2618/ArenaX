use chrono::{DateTime, Duration, Utc};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::AppError;
use crate::models::friend::{FriendRequest, FriendRequestStatus, Friendship};
use crate::services::notification_service::NotificationService;

/// Cooldown applied after a request is rejected or cancelled, preventing
/// immediate re-request spam between the same user pair.
const REQUEST_COOLDOWN_HOURS: i64 = 24;

pub struct FriendService;

impl FriendService {
    /// Send a friend request. Idempotent: a repeated or reciprocal request
    /// between the same pair resolves to the single existing active state
    /// instead of creating a duplicate row or notification.
    pub async fn send_request(
        pool: &PgPool,
        sender_id: Uuid,
        receiver_id: Uuid,
    ) -> Result<FriendRequest, AppError> {
        if sender_id == receiver_id {
            return Err(AppError::BadRequest("cannot friend yourself".into()));
        }

        let mut tx = pool.begin().await?;

        // Lock the pair deterministically to serialize concurrent/reciprocal
        // requests. Ordering the ids avoids deadlocks between mirrored calls.
        let (low, high) = if sender_id < receiver_id {
            (sender_id, receiver_id)
        } else {
            (receiver_id, sender_id)
        };
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
            .bind(format!("{low}:{high}"))
            .execute(&mut *tx)
            .await?;

        if Self::are_friends(&mut tx, sender_id, receiver_id).await? {
            return Err(AppError::Conflict("already friends".into()));
        }

        // Any existing request in either direction is the single active state.
        if let Some(existing) = Self::find_pair_request(&mut tx, sender_id, receiver_id).await? {
            return Self::resolve_existing(&mut tx, existing, sender_id, receiver_id).await;
        }

        // Enforce cooldown from a prior reject/cancel before creating a new row.
        if let Some(cooldown_until) = Self::active_cooldown(&mut tx, sender_id, receiver_id).await? {
            if cooldown_until > Utc::now() {
                return Err(AppError::TooManyRequests(
                    "friend request cooldown active".into(),
                ));
            }
        }

        let request = sqlx::query_as::<_, FriendRequest>(
            r#"INSERT INTO friend_requests (id, sender_id, receiver_id, status, created_at, updated_at)
               VALUES ($1, $2, $3, 'pending', NOW(), NOW())
               RETURNING *"#,
        )
        .bind(Uuid::new_v4())
        .bind(sender_id)
        .bind(receiver_id)
        .fetch_one(&mut *tx)
        .await?;

        NotificationService::create_friend_request(&mut tx, receiver_id, sender_id).await?;

        tx.commit().await?;
        Ok(request)
    }

    /// Accept a pending request. Only the receiver may accept; replaying an
    /// accept on an already-accepted request is a no-op returning the state.
    pub async fn accept_request(
        pool: &PgPool,
        request_id: Uuid,
        actor_id: Uuid,
    ) -> Result<FriendRequest, AppError> {
        let mut tx = pool.begin().await?;
        let request = Self::lock_request(&mut tx, request_id).await?;

        if request.receiver_id != actor_id {
            return Err(AppError::Forbidden("not the request receiver".into()));
        }

        match request.status {
            FriendRequestStatus::Accepted => {
                tx.commit().await?;
                return Ok(request);
            }
            FriendRequestStatus::Pending => {}
            _ => return Err(AppError::Conflict("request is no longer pending".into())),
        }

        let updated = Self::transition(&mut tx, request_id, FriendRequestStatus::Accepted).await?;

        sqlx::query(
            r#"INSERT INTO friendships (id, user_a, user_b, created_at)
               VALUES ($1, $2, $3, NOW())
               ON CONFLICT (user_a, user_b) DO NOTHING"#,
        )
        .bind(Uuid::new_v4())
        .bind(request.sender_id.min(request.receiver_id))
        .bind(request.sender_id.max(request.receiver_id))
        .execute(&mut *tx)
        .await?;

        NotificationService::create_friend_accepted(&mut tx, request.sender_id, actor_id).await?;

        tx.commit().await?;
        Ok(updated)
    }

    /// Reject a pending request. Only the receiver may reject. Replay is a no-op.
    pub async fn reject_request(
        pool: &PgPool,
        request_id: Uuid,
        actor_id: Uuid,
    ) -> Result<FriendRequest, AppError> {
        let mut tx = pool.begin().await?;
        let request = Self::lock_request(&mut tx, request_id).await?;

        if request.receiver_id != actor_id {
            return Err(AppError::Forbidden("not the request receiver".into()));
        }

        match request.status {
            FriendRequestStatus::Rejected => {
                tx.commit().await?;
                return Ok(request);
            }
            FriendRequestStatus::Pending => {}
            _ => return Err(AppError::Conflict("request is no longer pending".into())),
        }

        let updated = Self::transition(&mut tx, request_id, FriendRequestStatus::Rejected).await?;
        Self::start_cooldown(&mut tx, request.sender_id, request.receiver_id).await?;

        tx.commit().await?;
        Ok(updated)
    }

    /// Cancel a pending request. Only the sender may cancel. Replay is a no-op.
    pub async fn cancel_request(
        pool: &PgPool,
        request_id: Uuid,
        actor_id: Uuid,
    ) -> Result<FriendRequest, AppError> {
        let mut tx = pool.begin().await?;
        let request = Self::lock_request(&mut tx, request_id).await?;

        if request.sender_id != actor_id {
            return Err(AppError::Forbidden("not the request sender".into()));
        }

        match request.status {
            FriendRequestStatus::Cancelled => {
                tx.commit().await?;
                return Ok(request);
            }
            FriendRequestStatus::Pending => {}
            _ => return Err(AppError::Conflict("request is no longer pending".into())),
        }

        let updated = Self::transition(&mut tx, request_id, FriendRequestStatus::Cancelled).await?;
        Self::start_cooldown(&mut tx, request.sender_id, request.receiver_id).await?;

        tx.commit().await?;
        Ok(updated)
    }

    /// Read the request state for a pair, enforcing privacy before disclosure.
    /// Returns `None` when the viewer is not a participant or privacy forbids it.
    pub async fn get_pair_state(
        pool: &PgPool,
        viewer_id: Uuid,
        other_id: Uuid,
    ) -> Result<Option<FriendRequest>, AppError> {
        if viewer_id != other_id && !Self::can_disclose(pool, viewer_id, other_id).await? {
            return Ok(None);
        }

        let mut tx = pool.begin().await?;
        let state = Self::find_pair_request(&mut tx, viewer_id, other_id).await?;
        tx.commit().await?;
        Ok(state)
    }

    // --- helpers -----------------------------------------------------------

    async fn resolve_existing(
        tx: &mut Transaction<'_, Postgres>,
        existing: FriendRequest,
        sender_id: Uuid,
        receiver_id: Uuid,
    ) -> Result<FriendRequest, AppError> {
        match existing.status {
            FriendRequestStatus::Pending => {
                // Reciprocal request: the original receiver now requests back,
                // which is an implicit accept. Otherwise it is a replay.
                if existing.receiver_id == sender_id && existing.sender_id == receiver_id {
                    let updated =
                        Self::transition(tx, existing.id, FriendRequestStatus::Accepted).await?;
                    sqlx::query(
                        r#"INSERT INTO friendships (id, user_a, user_b, created_at)
                           VALUES ($1, $2, $3, NOW())
                           ON CONFLICT (user_a, user_b) DO NOTHING"#,
                    )
                    .bind(Uuid::new_v4())
                    .bind(sender_id.min(receiver_id))
                    .bind(sender_id.max(receiver_id))
                    .execute(&mut **tx)
                    .await?;
                    NotificationService::create_friend_accepted(
                        tx,
                        existing.sender_id,
                        sender_id,
                    )
                    .await?;
                    tx.commit().await?;
                    Ok(updated)
                } else {
                    // Replay of the same pending request: no duplicate row or
                    // notification, just return the current state.
                    tx.commit().await?;
                    Ok(existing)
                }
            }
            _ => Err(AppError::Conflict("request already resolved".into())),
        }
    }

    async fn transition(
        tx: &mut Transaction<'_, Postgres>,
        request_id: Uuid,
        status: FriendRequestStatus,
    ) -> Result<FriendRequest, AppError> {
        let updated = sqlx::query_as::<_, FriendRequest>(
            r#"UPDATE friend_requests
               SET status = $2, updated_at = NOW()
               WHERE id = $1
               RETURNING *"#,
        )
        .bind(request_id)
        .bind(status)
        .fetch_one(&mut **tx)
        .await?;
        Ok(updated)
    }

    async fn lock_request(
        tx: &mut Transaction<'_, Postgres>,
        request_id: Uuid,
    ) -> Result<FriendRequest, AppError> {
        sqlx::query_as::<_, FriendRequest>(
            "SELECT * FROM friend_requests WHERE id = $1 FOR UPDATE",
        )
        .bind(request_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| AppError::NotFound("friend request not found".into()))
    }

    async fn find_pair_request(
        tx: &mut Transaction<'_, Postgres>,
        a: Uuid,
        b: Uuid,
    ) -> Result<Option<FriendRequest>, AppError> {
        let request = sqlx::query_as::<_, FriendRequest>(
            r#"SELECT * FROM friend_requests
               WHERE (sender_id = $1 AND receiver_id = $2)
                  OR (sender_id = $2 AND receiver_id = $1)
               ORDER BY updated_at DESC
               LIMIT 1"#,
        )
        .bind(a)
        .bind(b)
        .fetch_optional(&mut **tx)
        .await?;
        Ok(request)
    }

    async fn are_friends(
        tx: &mut Transaction<'_, Postgres>,
        a: Uuid,
        b: Uuid,
    ) -> Result<bool, AppError> {
        let exists: Option<(i32,)> = sqlx::query_as(
            "SELECT 1 FROM friendships WHERE user_a = $1 AND user_b = $2",
        )
        .bind(a.min(b))
        .bind(a.max(b))
        .fetch_optional(&mut **tx)
        .await?;
        Ok(exists.is_some())
    }

    async fn start_cooldown(
        tx: &mut Transaction<'_, Postgres>,
        a: Uuid,
        b: Uuid,
    ) -> Result<(), AppError> {
        let until = Utc::now() + Duration::hours(REQUEST_COOLDOWN_HOURS);
        sqlx::query(
            r#"INSERT INTO friend_request_cooldowns (user_a, user_b, cooldown_until)
               VALUES ($1, $2, $3)
               ON CONFLICT (user_a, user_b)
               DO UPDATE SET cooldown_until = EXCLUDED.cooldown_until"#,
        )
        .bind(a.min(b))
        .bind(a.max(b))
        .bind(until)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    async fn active_cooldown(
        tx: &mut Transaction<'_, Postgres>,
        a: Uuid,
        b: Uuid,
    ) -> Result<Option<DateTime<Utc>>, AppError> {
        let row: Option<(DateTime<Utc>,)> = sqlx::query_as(
            "SELECT cooldown_until FROM friend_request_cooldowns WHERE user_a = $1 AND user_b = $2",
        )
        .bind(a.min(b))
        .bind(a.max(b))
        .fetch_optional(&mut **tx)
        .await?;
        Ok(row.map(|(until,)| until))
    }

    /// Privacy gate: only disclose request state when the target's privacy
    /// settings permit friend-request visibility to the viewer.
    async fn can_disclose(
        pool: &PgPool,
        viewer_id: Uuid,
        target_id: Uuid,
    ) -> Result<bool, AppError> {
        let row: Option<(bool,)> = sqlx::query_as(
            "SELECT friend_requests_visible FROM user_privacy_settings WHERE user_id = $1",
        )
        .bind(target_id)
        .fetch_optional(pool)
        .await?;
        // Default to private when no settings row exists.
        Ok(row.map(|(visible,)| visible).unwrap_or(false) && viewer_id != target_id)
    }
}
