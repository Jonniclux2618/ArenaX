use crate::api_error::ApiError;
use crate::auth::jwt_service::JwtService;
use bcrypt::{hash, verify, DEFAULT_COST};
use chrono::{DateTime, Utc};
use rand::RngCore;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

const RECOVERY_CODE_COUNT: usize = 10;

#[derive(Debug, Serialize)]
pub struct RecoveryCase {
    pub id: Uuid,
    pub user_id: Uuid,
    pub eligible_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct RecoveryCodeSet {
    pub codes: Vec<String>,
}

#[derive(Clone)]
pub struct AccountRecoveryService {
    db: PgPool,
    jwt: JwtService,
}

impl AccountRecoveryService {
    pub fn new(db: PgPool, jwt: JwtService) -> Self {
        Self { db, jwt }
    }

    pub async fn issue_codes(
        &self,
        user_id: Uuid,
        current_password: Option<&str>,
    ) -> Result<RecoveryCodeSet, ApiError> {
        let account: Option<Option<String>> =
            sqlx::query_scalar("SELECT password_hash FROM users WHERE id = $1 AND is_active = TRUE")
                .bind(user_id)
                .fetch_optional(&self.db)
                .await
                .map_err(ApiError::database_error)?;
        let password_hash = account.ok_or_else(|| ApiError::unauthorized("Invalid credentials"))?;
        if let Some(password_hash) = password_hash {
            let current_password = current_password
                .ok_or_else(|| ApiError::unauthorized("Current password required"))?;
            if !verify(current_password, &password_hash)
                .map_err(|error| ApiError::internal_error(error.to_string()))?
            {
                return Err(ApiError::unauthorized("Invalid credentials"));
            }
        }

        let mut codes = Vec::with_capacity(RECOVERY_CODE_COUNT);
        for _ in 0..RECOVERY_CODE_COUNT {
            let mut secret = [0u8; 32];
            rand::thread_rng().fill_bytes(&mut secret);
            codes.push(hex::encode(secret));
        }

        let mut tx = self.db.begin().await.map_err(ApiError::database_error)?;
        let active_user: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM users WHERE id = $1 AND is_active = TRUE AND deleted_at IS NULL FOR UPDATE",
        )
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(ApiError::database_error)?;
        if active_user.is_none() {
            return Err(ApiError::unauthorized("Invalid credentials"));
        }
        sqlx::query(
            r#"
            WITH expired AS (
                UPDATE account_recovery_cases
                SET status = 'expired'
                WHERE user_id = $1 AND status = 'pending' AND expires_at <= NOW()
                RETURNING id
            )
            INSERT INTO account_recovery_case_audit (case_id, action)
            SELECT id, 'case_expired' FROM expired
            "#,
        )
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::database_error)?;
        let cancelled: Vec<Uuid> = sqlx::query_scalar(
            r#"
            UPDATE account_recovery_cases
            SET status = 'cancelled'
            WHERE user_id = $1 AND status IN ('pending', 'appealed')
            RETURNING id
            "#,
        )
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await
        .map_err(ApiError::database_error)?;
        for case_id in cancelled {
            sqlx::query(
                "INSERT INTO account_recovery_case_audit (case_id, action) VALUES ($1, 'codes_rotated')",
            )
            .bind(case_id)
            .execute(&mut *tx)
            .await
            .map_err(ApiError::database_error)?;
        }
        sqlx::query(
            "UPDATE account_recovery_codes SET consumed_at = NOW() WHERE user_id = $1 AND consumed_at IS NULL",
        )
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::database_error)?;
        for code in &codes {
            sqlx::query(
                "INSERT INTO account_recovery_codes (user_id, code_hash) VALUES ($1, $2)",
            )
            .bind(user_id)
            .bind(hash_code(code))
            .execute(&mut *tx)
            .await
            .map_err(ApiError::database_error)?;
        }
        sqlx::query(
            "INSERT INTO audit_logs (user_id, action, resource_type, resource_id, details) VALUES ($1, 'recovery_codes_issued', 'account_recovery', $1, '{\"count\":10}')",
        )
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::database_error)?;
        tx.commit().await.map_err(ApiError::database_error)?;

        Ok(RecoveryCodeSet { codes })
    }

    pub async fn start_case(&self, code: &str) -> Result<Option<RecoveryCase>, ApiError> {
        if code.len() != 64 || !code.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Ok(None);
        }
        let mut tx = self.db.begin().await.map_err(ApiError::database_error)?;
        let credential: Option<(Uuid, Uuid)> = sqlx::query_as(
            r#"
            SELECT code.id, code.user_id
            FROM account_recovery_codes code
            JOIN users account ON account.id = code.user_id
            WHERE code.code_hash = $1 AND code.consumed_at IS NULL
              AND code.expires_at > NOW() AND account.is_active = TRUE
              AND account.deleted_at IS NULL
            FOR UPDATE OF code
            "#,
        )
        .bind(hash_code(code))
        .fetch_optional(&mut *tx)
        .await
        .map_err(ApiError::database_error)?;
        let Some((code_id, user_id)) = credential else {
            tx.commit().await.map_err(ApiError::database_error)?;
            return Ok(None);
        };

        sqlx::query(
            r#"
            WITH expired AS (
                UPDATE account_recovery_cases
                SET status = 'expired'
                WHERE user_id = $1 AND status = 'pending' AND expires_at <= NOW()
                RETURNING id
            )
            INSERT INTO account_recovery_case_audit (case_id, action)
            SELECT id, 'case_expired' FROM expired
            "#,
        )
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::database_error)?;
        let appealed: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM account_recovery_cases WHERE user_id = $1 AND status = 'appealed' AND expires_at > NOW())",
        )
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(ApiError::database_error)?;
        if appealed {
            tx.commit().await.map_err(ApiError::database_error)?;
            return Ok(None);
        }

        let case: (Uuid, DateTime<Utc>, DateTime<Utc>) = sqlx::query_as(
            r#"
            INSERT INTO account_recovery_cases (user_id, recovery_code_id)
            VALUES ($1, $2)
            ON CONFLICT (user_id) WHERE status = 'pending'
            DO UPDATE SET recovery_code_id = EXCLUDED.recovery_code_id
            RETURNING id, eligible_at, expires_at
            "#,
        )
        .bind(user_id)
        .bind(code_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(ApiError::database_error)?;
        sqlx::query(
            r#"
            INSERT INTO account_recovery_case_audit (case_id, action)
            VALUES ($1, 'request_received')
            "#,
        )
        .bind(case.0)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::database_error)?;
        sqlx::query(
            "UPDATE account_recovery_codes SET consumed_at = COALESCE(consumed_at, NOW()) WHERE id = $1",
        )
        .bind(code_id)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::database_error)?;
        tx.commit().await.map_err(ApiError::database_error)?;

        Ok(Some(RecoveryCase {
            id: case.0,
            user_id,
            eligible_at: case.1,
            expires_at: case.2,
        }))
    }

    pub async fn record_notification_result(
        &self,
        case_id: Uuid,
        delivered: bool,
    ) -> Result<(), ApiError> {
        let status = if delivered { "sent" } else { "failed" };
        let mut tx = self.db.begin().await.map_err(ApiError::database_error)?;
        sqlx::query(
            "UPDATE account_recovery_cases SET notification_status = $2 WHERE id = $1 AND status = 'pending'",
        )
        .bind(case_id)
        .bind(status)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::database_error)?;
        sqlx::query(
            "INSERT INTO account_recovery_case_audit (case_id, action, details) VALUES ($1, 'notification_' || $2, '{}'::jsonb)",
        )
        .bind(case_id)
        .bind(status)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::database_error)?;
        tx.commit().await.map_err(ApiError::database_error)
    }

    pub async fn complete_case(
        &self,
        case_id: Uuid,
        code: &str,
        new_password: &str,
    ) -> Result<Uuid, ApiError> {
        if !(12..=72).contains(&new_password.len()) {
            return Err(ApiError::bad_request(
                "New password must be between 12 and 72 bytes",
            ));
        }
        if code.len() != 64 || !code.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ApiError::unauthorized("Recovery proof is invalid or expired"));
        }
        let password_hash =
            hash(new_password, DEFAULT_COST).map_err(|error| ApiError::internal_error(error.to_string()))?;
        let mut tx = self.db.begin().await.map_err(ApiError::database_error)?;
        let recovery: Option<Uuid> = sqlx::query_scalar(
            r#"
            SELECT code.user_id
            FROM account_recovery_codes code
            JOIN account_recovery_cases recovery
              ON recovery.recovery_code_id = code.id AND recovery.user_id = code.user_id
            WHERE recovery.id = $1 AND code.code_hash = $2
              AND recovery.status = 'pending'
              AND recovery.eligible_at <= NOW()
              AND recovery.expires_at > NOW()
              AND code.consumed_at IS NOT NULL AND code.expires_at > NOW()
            FOR UPDATE OF code, recovery
            "#,
        )
        .bind(case_id)
        .bind(hash_code(code))
        .fetch_optional(&mut *tx)
        .await
        .map_err(ApiError::database_error)?;
        let Some(user_id) = recovery else {
            return Err(ApiError::unauthorized(
                "Recovery proof is invalid, expired, or still cooling down",
            ));
        };

        self.jwt
            .revoke_user_sessions(user_id)
            .await
            .map_err(|error| ApiError::internal_error(format!("Session revocation failed: {error}")))?;
        let updated = sqlx::query(
            "UPDATE users SET password_hash = $1, updated_at = NOW() WHERE id = $2 AND is_active = TRUE AND deleted_at IS NULL",
        )
        .bind(password_hash)
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::database_error)?;
        if updated.rows_affected() != 1 {
            return Err(ApiError::unauthorized("Account is no longer eligible for recovery"));
        }
        sqlx::query(
            "UPDATE account_recovery_codes SET consumed_at = NOW() WHERE user_id = $1 AND consumed_at IS NULL",
        )
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::database_error)?;
        sqlx::query(
            "UPDATE account_recovery_cases SET status = 'completed', completed_at = NOW() WHERE id = $1",
        )
        .bind(case_id)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::database_error)?;
        sqlx::query(
            "INSERT INTO account_recovery_case_audit (case_id, action) VALUES ($1, 'recovery_completed')",
        )
        .bind(case_id)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::database_error)?;
        sqlx::query(
            "INSERT INTO audit_logs (user_id, action, resource_type, resource_id) VALUES ($1, 'account_recovered', 'account_recovery', $2)",
        )
        .bind(user_id)
        .bind(case_id)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::database_error)?;
        tx.commit().await.map_err(ApiError::database_error)?;
        Ok(user_id)
    }

    pub async fn appeal_case(&self, case_id: Uuid, code: &str) -> Result<(), ApiError> {
        let mut tx = self.db.begin().await.map_err(ApiError::database_error)?;
        let result = sqlx::query(
            r#"
            UPDATE account_recovery_cases recovery
            SET status = 'appealed'
            FROM account_recovery_codes code
            WHERE recovery.id = $1 AND recovery.recovery_code_id = code.id
              AND code.code_hash = $2 AND code.expires_at > NOW()
              AND recovery.status = 'pending' AND recovery.expires_at > NOW()
            "#,
        )
        .bind(case_id)
        .bind(hash_code(code))
        .execute(&mut *tx)
        .await
        .map_err(ApiError::database_error)?;
        if result.rows_affected() == 0 {
            return Err(ApiError::unauthorized("Recovery proof is invalid or expired"));
        }
        sqlx::query(
            "INSERT INTO account_recovery_case_audit (case_id, action) VALUES ($1, 'appeal_requested')",
        )
        .bind(case_id)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::database_error)?;
        tx.commit().await.map_err(ApiError::database_error)
    }

    pub async fn resolve_appeal(
        &self,
        case_id: Uuid,
        reviewer: Uuid,
        approve: bool,
    ) -> Result<Uuid, ApiError> {
        let mut tx = self.db.begin().await.map_err(ApiError::database_error)?;
        let status = if approve { "pending" } else { "cancelled" };
        let user_id: Option<Uuid> = sqlx::query_scalar(
            r#"
            UPDATE account_recovery_cases
            SET status = $2
            WHERE id = $1 AND status = 'appealed' AND expires_at > NOW()
            RETURNING user_id
            "#,
        )
        .bind(case_id)
        .bind(status)
        .fetch_optional(&mut *tx)
        .await
        .map_err(ApiError::database_error)?;
        let user_id = user_id.ok_or_else(|| ApiError::not_found("Active appealed case not found"))?;
        let action = if approve { "appeal_approved" } else { "appeal_denied" };
        sqlx::query(
            "INSERT INTO account_recovery_case_audit (case_id, action, details) VALUES ($1, $2, jsonb_build_object('reviewer_id', $3))",
        )
        .bind(case_id)
        .bind(action)
        .bind(reviewer)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::database_error)?;
        tx.commit().await.map_err(ApiError::database_error)?;
        Ok(user_id)
    }
}

fn hash_code(code: &str) -> String {
    hex::encode(Sha256::digest(code.as_bytes()))
}
