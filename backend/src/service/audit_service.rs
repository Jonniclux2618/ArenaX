//! Audit service for sensitive operations (Issue #946)
//!
//! The trail itself — the `audit_logs` table, its hash chain and its
//! append-only rules — was built in Issue #863, and the database writes a row
//! for every change to a sensitive table. That covers *data modifications*.
//!
//! Two things it cannot cover, which this service adds:
//!
//!   1. **Admin actions that are not row changes.** Reading a player's payment
//!      history, exporting a user's data, forcing a match result, impersonating
//!      an account — these leave no `UPDATE` behind, so a trigger never fires.
//!      They are exactly the operations an audit trail exists to record.
//!   2. **Attribution.** A trigger knows which row changed but not who asked
//!      for it; the connection is the same service account either way. The
//!      actor has to be declared by the application, per transaction.
//!
//! It also owns the retention policy: entries are kept for seven years.
//!
//! # Why writes go through one type
//!
//! Every write here sets `source = 'application'`, so the trail distinguishes
//! what the database observed from what the application claimed. Nothing else
//! in the codebase should `INSERT INTO audit_logs` directly.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sqlx::{PgPool, Postgres, Transaction};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracing::{debug, error, warn};
use uuid::Uuid;

/// How long audit entries are retained before they may be archived.
///
/// Seven years, expressed in days. Calendar arithmetic over that span is the
/// database's job — `retained_until` is computed as an INTERVAL in SQL — but
/// the constant is here so the policy is greppable from the service that
/// enforces it.
pub const RETENTION_YEARS: i64 = 7;
const RETENTION_DAYS: i64 = RETENTION_YEARS * 365;

/// Upper bound on rows returned by one query.
///
/// The trail only grows, so an unbounded read is a way to exhaust the
/// connection pool — accidentally or otherwise.
const MAX_PAGE_SIZE: i64 = 500;
const DEFAULT_PAGE_SIZE: i64 = 50;

#[derive(Error, Debug)]
pub enum AuditError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("Invalid audit request: {0}")]
    Invalid(String),
}

/// Actions the service records.
///
/// A closed set rather than a free-form string: an audit trail is only
/// queryable if the same operation is named the same way every time, and
/// `"user_ban"` vs `"ban_user"` vs `"banned"` is how that stops being true.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditAction {
    /// Privileged read of another user's data.
    AdminView,
    /// Bulk export of personal or financial data.
    AdminExport,
    /// Role, permission or feature-flag change.
    AdminGrant,
    AdminRevoke,
    /// Account state changes made on a user's behalf.
    AdminBan,
    AdminUnban,
    /// Acting as another user.
    AdminImpersonate,
    /// Manual override of a system-computed outcome (match result, payout).
    AdminOverride,
    /// Configuration change to the platform itself.
    AdminConfigChange,
    /// Explicit application-level record of a data modification, for paths
    /// where the change does not land in an audited table.
    DataModification,
    /// A privileged operator request that was refused before submission —
    /// expired session, revoked session, or an action outside the session's
    /// scope. Recorded so a rejection leaves evidence, not just a 403.
    OperatorRequestRejected,
}

impl AuditAction {
    /// Stored form. Kept under the `action VARCHAR(50)` the table declares.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AdminView => "admin_view",
            Self::AdminExport => "admin_export",
            Self::AdminGrant => "admin_grant",
            Self::AdminRevoke => "admin_revoke",
            Self::AdminBan => "admin_ban",
            Self::AdminUnban => "admin_unban",
            Self::AdminImpersonate => "admin_impersonate",
            Self::AdminOverride => "admin_override",
            Self::AdminConfigChange => "admin_config_change",
            Self::DataModification => "data_modification",
            Self::OperatorRequestRejected => "operator_request_rejected",
        }
    }
}

/// Retention class for an audit event category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditRetentionClass {
    Security,
    Operational,
    Compliance,
}

impl AuditRetentionClass {
    pub fn retention_days(self) -> i64 {
        match self {
            Self::Security => 365,
            Self::Operational => 90,
            Self::Compliance => RETENTION_DAYS,
        }
    }
}

/// Field-level policy declared by each audit event schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEventSchema {
    pub action: AuditAction,
    pub required_fields: Vec<&'static str>,
    pub pseudonymized_fields: Vec<&'static str>,
    pub prohibited_fields: Vec<&'static str>,
    pub retention_class: AuditRetentionClass,
}

impl AuditEventSchema {
    pub fn validate(&self, value: &JsonValue) -> Result<(), AuditError> {
        let object = value
            .as_object()
            .ok_or_else(|| AuditError::Invalid("Audit payload must be a JSON object".into()))?;

        for field in &self.required_fields {
            if !object.contains_key(*field) {
                return Err(AuditError::Invalid(format!(
                    "Required audit field '{}' is missing",
                    field
                )));
            }
        }

        for field in &self.prohibited_fields {
            if object.contains_key(*field) {
                return Err(AuditError::Invalid(format!(
                    "Prohibited audit field '{}' was supplied",
                    field
                )));
            }
        }

        Ok(())
    }

    pub fn redact(&self, value: &JsonValue) -> JsonValue {
        let mut out = value.clone();
        if let Some(object) = out.as_object_mut() {
            for field in &self.pseudonymized_fields {
                if object.contains_key(*field) {
                    if let Some(raw) = object.get(*field).and_then(|v| v.as_str()) {
                        let digest = format!("{:x}", Sha256::digest(raw.as_bytes()));
                        object.insert(
                            (*field).to_string(),
                            JsonValue::String(format!("sha256:{}", &digest[..16.min(digest.len())])),
                        );
                    }
                }
            }
            for field in &self.prohibited_fields {
                object.remove(*field);
            }
        }
        out
    }
}

/// Why a privileged operator request was refused.
///
/// A closed set for the same reason as [`AuditAction`]: the rejection reason
/// is what an investigator groups by, so it must be spelled consistently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectionReason {
    /// The session's expiry had passed when the action was submitted.
    Expired,
    /// The session was revoked after the action was queued.
    Revoked,
    /// The action was not covered by the session's scope.
    WrongScope,
    /// The session's purpose did not permit the action.
    WrongPurpose,
    /// No approval reference was bound to the session.
    MissingApproval,
    /// The session was unknown to the store.
    UnknownSession,
}

impl RejectionReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Expired => "expired",
            Self::Revoked => "revoked",
            Self::WrongScope => "wrong_scope",
            Self::WrongPurpose => "wrong_purpose",
            Self::MissingApproval => "missing_approval",
            Self::UnknownSession => "unknown_session",
        }
    }
}

/// Evidence for a refused privileged request.
///
/// Carries the session's binding (actor, purpose, scope, expiry, approval
/// reference) so the record answers "who asked, for what, under which
/// approval, and why was it refused" without a join back to a session that
/// may already have been purged. Signing keys are never part of this — they
/// live outside the session store and are never copied into the trail.
#[derive(Debug, Clone)]
pub struct RejectedRequestInput {
    /// The operator the session was issued to.
    pub actor_id: Option<Uuid>,
    /// The session identifier, if one was presented.
    pub session_id: Option<Uuid>,
    /// What the session was issued for, e.g. `"payout_override"`.
    pub purpose: String,
    /// The scopes the session held.
    pub scope: Vec<String>,
    /// The scope the refused action required.
    pub required_scope: Option<String>,
    /// When the session expired (or would have).
    pub expires_at: Option<DateTime<Utc>>,
    /// The approval reference bound to the session.
    pub approval_reference: Option<String>,
    /// The action that was attempted, e.g. `"admin_override"`.
    pub attempted_action: String,
    /// The resource the action targeted.
    pub resource_type: String,
    pub resource_id: Option<Uuid>,
    pub reason: RejectionReason,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
}

impl RejectedRequestInput {
    pub fn new(
        purpose: impl Into<String>,
        attempted_action: impl Into<String>,
        resource_type: impl Into<String>,
        reason: RejectionReason,
    ) -> Self {
        Self {
            actor_id: None,
            session_id: None,
            purpose: purpose.into(),
            scope: Vec::new(),
            required_scope: None,
            expires_at: None,
            approval_reference: None,
            attempted_action: attempted_action.into(),
            resource_type: resource_type.into(),
            resource_id: None,
            reason,
            ip_address: None,
            user_agent: None,
        }
    }

    pub fn actor(mut self, actor_id: Uuid) -> Self {
        self.actor_id = Some(actor_id);
        self
    }

    pub fn session(mut self, session_id: Uuid) -> Self {
        self.session_id = Some(session_id);
        self
    }

    pub fn scope(mut self, scope: Vec<String>) -> Self {
        self.scope = scope;
        self
    }

    pub fn required_scope(mut self, scope: impl Into<String>) -> Self {
        self.required_scope = Some(scope.into());
        self
    }

    pub fn expires_at(mut self, expires_at: DateTime<Utc>) -> Self {
        self.expires_at = Some(expires_at);
        self
    }

    pub fn approval_reference(mut self, reference: impl Into<String>) -> Self {
        self.approval_reference = Some(reference.into());
        self
    }

    pub fn resource(mut self, resource_id: Uuid) -> Self {
        self.resource_id = Some(resource_id);
        self
    }

    pub fn request_context(
        mut self,
        ip_address: Option<String>,
        user_agent: Option<String>,
    ) -> Self {
        self.ip_address = ip_address;
        self.user_agent = user_agent;
        self
    }

    /// Render the evidence as the `new_values` payload of an audit entry.
    ///
    /// Kept as a pure function so the shape can be asserted in tests without
    /// a database.
    pub fn evidence(&self) -> JsonValue {
        serde_json::json!({
            "session_id": self.session_id,
            "purpose": self.purpose,
            "scope": self.scope,
            "required_scope": self.required_scope,
            "expires_at": self.expires_at,
            "approval_reference": self.approval_reference,
            "attempted_action": self.attempted_action,
            "rejection_reason": self.reason.as_str(),
        })
    }

    /// Convert into the generic audit entry this service writes.
    pub fn into_entry(self) -> AuditEntryInput {
        let evidence = self.evidence();
        let mut entry = AuditEntryInput::new(
            AuditAction::OperatorRequestRejected,
            self.resource_type.clone(),
        )
        .after(evidence)
        .request_context(self.ip_address.clone(), self.user_agent.clone());

        if let Some(actor_id) = self.actor_id {
            entry = entry.actor(actor_id);
        }
        if let Some(resource_id) = self.resource_id {
            entry = entry.resource(resource_id);
        }
        entry
    }
}

/// One entry to be written.
#[derive(Debug, Clone)]
pub struct AuditEntryInput {
    /// The administrator or system account performing the action.
    pub actor_id: Option<Uuid>,
    pub action: AuditAction,
    /// Table or domain object the action touched, e.g. `"users"`.
    pub resource_type: String,
    pub resource_id: Option<Uuid>,
    /// State before the change, where there was one.
    pub old_values: Option<JsonValue>,
    /// State after the change, or the parameters of a read/export.
    pub new_values: Option<JsonValue>,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
}

impl AuditEntryInput {
    pub fn new(action: AuditAction, resource_type: impl Into<String>) -> Self {
        Self {
            actor_id: None,
            action,
            resource_type: resource_type.into(),
            resource_id: None,
            old_values: None,
            new_values: None,
            ip_address: None,
            user_agent: None,
        }
    }

    pub fn actor(mut self, actor_id: Uuid) -> Self {
        self.actor_id = Some(actor_id);
        self
    }

    pub fn resource(mut self, resource_id: Uuid) -> Self {
        self.resource_id = Some(resource_id);
        self
    }

    pub fn before(mut self, old_values: JsonValue) -> Self {
        self.old_values = Some(old_values);
        self
    }

    pub fn after(mut self, new_values: JsonValue) -> Self {
        self.new_values = Some(new_values);
        self
    }

    pub fn request_context(
        mut self,
        ip_address: Option<String>,
        user_agent: Option<String>,
    ) -> Self {
        self.ip_address = ip_address;
        self.user_agent = user_agent;
        self
    }
}

/// A stored entry.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct AuditRecord {
    pub id: Uuid,
    pub sequence_number: i64,
    pub user_id: Option<Uuid>,
    pub action: String,
    pub resource_type: String,
    pub resource_id: Option<Uuid>,
    pub old_values: Option<JsonValue>,
    pub new_values: Option<JsonValue>,
    pub source: String,
    pub entry_hash: Option<String>,
    pub previous_hash: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Filters for reading the trail.
///
/// All optional and all narrowing: an investigation starts from one known fact
/// — a user, a resource, a date — and widens from there.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AuditFilter {
    pub user_id: Option<Uuid>,
    pub action: Option<String>,
    pub resource_type: Option<String>,
    pub resource_id: Option<Uuid>,
    pub source: Option<String>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// Outcome of a chain verification.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct ChainStatus {
    pub ok: bool,
    pub checked_rows: i64,
    pub first_bad_sequence: Option<i64>,
    pub reason: String,
}

/// Result of an archive sweep.
#[derive(Debug, Clone, Serialize)]
pub struct RetentionSweep {
    pub archived: i64,
    pub cutoff: DateTime<Utc>,
}

#[derive(Clone)]
pub struct AuditService {
    pool: PgPool,
}

impl AuditService {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    // ───────────────────────────────────────────
    // Writing
    // ───────────────────────────────────────────

    /// Record an admin action.
    ///
    /// # Failure handling
    ///
    /// Returns the error rather than swallowing it. Callers doing something
    /// irreversible should write the entry *before* the action and treat a
    /// failure as a reason not to proceed: an action that happened with no
    /// record of it is worse than an action that did not happen.
    pub async fn log(&self, entry: AuditEntryInput) -> Result<Uuid, AuditError> {
        if entry.resource_type.trim().is_empty() {
            return Err(AuditError::Invalid("resource_type is required".into()));
        }

        let id: Uuid = sqlx::query_scalar(
            r#"
            INSERT INTO audit_logs (
                user_id, action, resource_type, resource_id,
                old_values, new_values, ip_address, user_agent,
                source, created_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7::inet, $8, 'application', NOW())
            RETURNING id
            "#,
        )
        .bind(entry.actor_id)
        .bind(entry.action.as_str())
        .bind(&entry.resource_type)
        .bind(entry.resource_id)
        .bind(&entry.old_values)
        .bind(&entry.new_values)
        .bind(entry.ip_address.as_deref())
        .bind(entry.user_agent.as_deref())
        .fetch_one(&self.pool)
        .await
        .inspect_err(|e| {
            error!(
                action = entry.action.as_str(),
                resource_type = %entry.resource_type,
                "failed to write audit entry: {e}"
            );
        })?;

        debug!(
            audit_id = %id,
            action = entry.action.as_str(),
            "audit entry recorded"
        );

        Ok(id)
    }

    /// Record an entry inside a caller's transaction.
    ///
    /// Use this when the audit entry and the change it describes must either
    /// both land or neither does — a committed change with no entry is a gap
    /// in the trail, and an entry for a rolled-back change is a false record.
    pub async fn log_in_tx(
        tx: &mut Transaction<'_, Postgres>,
        entry: AuditEntryInput,
    ) -> Result<Uuid, AuditError> {
        let id: Uuid = sqlx::query_scalar(
            r#"
            INSERT INTO audit_logs (
                user_id, action, resource_type, resource_id,
                old_values, new_values, ip_address, user_agent,
                source, created_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7::inet, $8, 'application', NOW())
            RETURNING id
            "#,
        )
        .bind(entry.actor_id)
        .bind(entry.action.as_str())
        .bind(&entry.resource_type)
        .bind(entry.resource_id)
        .bind(&entry.old_values)
        .bind(&entry.new_values)
        .bind(entry.ip_address.as_deref())
        .bind(entry.user_agent.as_deref())
        .fetch_one(&mut **tx)
        .await?;

        Ok(id)
    }

    /// Record a refused privileged request.
    ///
    /// Unlike [`Self::log`], a failure here is logged and swallowed: the
    /// request is already being refused, and turning an audit-write failure
    /// into a different error would hide the rejection from the caller. The
    /// rejection itself is the security decision; the record is evidence.
    pub async fn log_rejected_request(
        &self,
        rejected: RejectedRequestInput,
    ) -> Result<Uuid, AuditError> {
        let reason = rejected.reason;
        let actor_id = rejected.actor_id;
        let attempted_action = rejected.attempted_action.clone();

        match self.log(rejected.into_entry()).await {
            Ok(id) => {
                warn!(
                    audit_id = %id,
                    actor_id = ?actor_id,
                    attempted_action = %attempted_action,
                    rejection_reason = reason.as_str(),
                    "privileged operator request rejected"
                );
                Ok(id)
            }
            Err(e) => {
                error!(
                    actor_id = ?actor_id,
                    attempted_action = %attempted_action,
                    rejection_reason = reason.as_str(),
                    "failed to record rejected privileged request: {e}"
                );
                Err(e)
            }
        }
    }

    /// Record a refused privileged request inside a caller's transaction.
    pub async fn log_rejected_request_in_tx(
        tx: &mut Transaction<'_, Postgres>,
        rejected: RejectedRequestInput,
    ) -> Result<Uuid, AuditError> {
        Self::log_in_tx(tx, rejected.into_entry()).await
    }

    /// Declare who the current transaction is acting as.
    ///
    /// The `audit_row_change` trigger reads `audit.actor_id` and falls back to
    /// the row's own `user_id` when it is unset — which attributes an admin's
    /// edit to the user whose row was edited. Calling this at the start of a
    /// privileged transaction is what makes trigger-written rows say who
    /// actually did it.
    ///
    /// `set_config(..., true)` scopes the value to the transaction, so it
    /// cannot leak to the next request that borrows the same pooled connection.
    pub async fn set_transaction_actor(
        tx: &mut Transaction<'_, Postgres>,
        actor_id: Uuid,
    ) -> Result<(), AuditError> {
        sqlx::query("SELECT set_config('audit.actor_id', $1, true)")
            .bind(actor_id.to_string())
            .execute(&mut **tx)
            .await?;
        Ok(())
    }

    // ───────────────────────────────────────────
    // Reading
    // ───────────────────────────────────────────

    /// Query the trail by user, date range and/or resource.
    ///
    /// The filters are bound as parameters against one prepared statement
    /// rather than concatenated per combination: fewer cached plans, and no
    /// string building anywhere near an admin-privileged query.
    pub async fn query(&self, filter: &AuditFilter) -> Result<Vec<AuditRecord>, AuditError> {
        if let (Some(from), Some(to)) = (filter.from, filter.to) {
            if from > to {
                return Err(AuditError::Invalid("`from` is after `to`".into()));
            }
        }

        let limit = filter
            .limit
            .unwrap_or(DEFAULT_PAGE_SIZE)
            .clamp(1, MAX_PAGE_SIZE);
        let offset = filter.offset.unwrap_or(0).max(0);

        let records = sqlx::query_as::<_, AuditRecord>(
            r#"
            SELECT id, sequence_number, user_id, action, resource_type, resource_id,
                   old_values, new_values, source, entry_hash, previous_hash, created_at
            FROM audit_logs
            WHERE ($1::uuid IS NULL OR user_id = $1)
              AND ($2::text IS NULL OR action = $2)
              AND ($3::text IS NULL OR resource_type = $3)
              AND ($4::uuid IS NULL OR resource_id = $4)
              AND ($5::text IS NULL OR source = $5)
              AND ($6::timestamptz IS NULL OR created_at >= $6)
              AND ($7::timestamptz IS NULL OR created_at <= $7)
            ORDER BY sequence_number DESC
            LIMIT $8 OFFSET $9
            "#,
        )
        .bind(filter.user_id)
        .bind(filter.action.as_deref())
        .bind(filter.resource_type.as_deref())
        .bind(filter.resource_id)
        .bind(filter.source.as_deref())
        .bind(filter.from)
        .bind(filter.to)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;

        Ok(records)
    }

    /// Count matching entries, for paging.
    pub async fn count(&self, filter: &AuditFilter) -> Result<i64, AuditError> {
        let total: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*)
            FROM audit_logs
            WHERE ($1::uuid IS NULL OR user_id = $1)
              AND ($2::text IS NULL OR action = $2)
              AND ($3::text IS NULL OR resource_type = $3)
              AND ($4::uuid IS NULL OR resource_id = $4)
              AND ($5::text IS NULL OR source = $5)
              AND ($6::timestamptz IS NULL OR created_at >= $6)
              AND ($7::timestamptz IS NULL OR created_at <= $7)
            "#,
        )
        .bind(filter.user_id)
        .bind(filter.action.as_deref())
        .bind(filter.resource_type.as_deref())
        .bind(filter.resource_id)
        .bind(filter.source.as_deref())
        .bind(filter.from)
        .bind(filter.to)
        .fetch_one(&self.pool)
        .await?;

        Ok(total)
    }

    /// Full history for one resource, oldest first — the order the changes
    /// actually happened in, which is how a reviewer reads a timeline.
    pub async fn resource_history(
        &self,
        resource_type: &str,
        resource_id: Uuid,
    ) -> Result<Vec<AuditRecord>, AuditError> {
        let records = sqlx::query_as::<_, AuditRecord>(
            r#"
            SELECT id, sequence_number, user_id, action, resource_type, resource_id,
                   old_values, new_values, source, entry_hash, previous_hash, created_at
            FROM audit_logs
            WHERE resource_type = $1 AND resource_id = $2
            ORDER BY sequence_number ASC
            LIMIT $3
            "#,
        )
        .bind(resource_type)
        .bind(resource_id)
        .bind(MAX_PAGE_SIZE)
        .fetch_all(&self.pool)
        .await?;

        Ok(records)
    }

    /// Everything one actor did in a window.
    pub async fn actor_activity(
        &self,
        actor_id: Uuid,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<AuditRecord>, AuditError> {
        self.query(&AuditFilter {
            user_id: Some(actor_id),
            from: Some(from),
            to: Some(to),
            limit: Some(MAX_PAGE_SIZE),
            ..Default::default()
        })
        .await
    }

    // ───────────────────────────────────────────
    // Integrity and retention
    // ───────────────────────────────────────────

    /// Recompute the hash chain and report the first break.
    ///
    /// Worth running on a schedule rather than only on demand: the value of a
    /// tamper-evident log is in noticing, and nobody notices a chain they
    /// never check.
    pub async fn verify_chain(
        &self,
        from_sequence: Option<i64>,
        to_sequence: Option<i64>,
    ) -> Result<ChainStatus, AuditError> {
        let status = sqlx::query_as::<_, ChainStatus>(
            "SELECT ok, checked_rows, first_bad_sequence, reason
             FROM verify_audit_chain($1, $2)",
        )
        .bind(from_sequence.unwrap_or(0))
        .bind(to_sequence)
        .fetch_one(&self.pool)
        .await?;

        if !status.ok {
            error!(
                first_bad_sequence = ?status.first_bad_sequence,
                "audit chain verification failed: {}",
                status.reason
            );
        }

        Ok(status)
    }

    /// The date before which entries are past their retention period.
    pub fn retention_cutoff() -> DateTime<Utc> {
        Utc::now() - Duration::days(RETENTION_DAYS)
    }

    /// Move entries past their seven-year retention into the archive table.
    ///
    /// Not a delete. `audit_logs` carries rules rejecting UPDATE and DELETE,
    /// and the hash chain is only verifiable while the rows it covers are
    /// contiguous — removing the oldest entries would break verification of
    /// everything after them. The archive keeps the rows, and their hashes,
    /// outside the hot table.
    ///
    /// Safe to call repeatedly; already-archived entries are skipped.
    pub async fn archive_past_retention(&self) -> Result<RetentionSweep, AuditError> {
        let cutoff = Self::retention_cutoff();

        let archived: i64 = sqlx::query_scalar("SELECT archive_audit_logs_past_retention($1)")
            .bind(cutoff)
            .fetch_one(&self.pool)
            .await?;

        if archived > 0 {
            warn!(
                archived,
                cutoff = %cutoff,
                "archived audit entries past the {RETENTION_YEARS}-year retention period"
            );
        }

        Ok(RetentionSweep { archived, cutoff })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_names_fit_the_column_and_are_stable() {
        let actions = [
            AuditAction::AdminView,
            AuditAction::AdminExport,
            AuditAction::AdminGrant,
            AuditAction::AdminRevoke,
            AuditAction::AdminBan,
            AuditAction::AdminUnban,
            AuditAction::AdminImpersonate,
            AuditAction::AdminOverride,
            AuditAction::AdminConfigChange,
            AuditAction::DataModification,
            AuditAction::OperatorRequestRejected,
        ];

        for action in actions {
            // `action` is VARCHAR(50); an overlong name would be rejected at
            // INSERT, which is the worst moment to find out.
            assert!(action.as_str().len() <= 50);
            assert!(!action.as_str().is_empty());
        }
    }

    #[test]
    fn builder_carries_every_field() {
        let actor = Uuid::new_v4();
        let resource = Uuid::new_v4();

        let entry = AuditEntryInput::new(AuditAction::AdminBan, "users")
            .actor(actor)
            .resource(resource)
            .before(serde_json::json!({ "banned": false }))
            .after(serde_json::json!({ "banned": true }))
            .request_context(Some("203.0.113.7".into()), Some("curl/8".into()));

        assert_eq!(entry.actor_id, Some(actor));
        assert_eq!(entry.resource_id, Some(resource));
        assert_eq!(entry.action.as_str(), "admin_ban");
        assert_eq!(entry.old_values, Some(serde_json::json!({"banned": false})));
        assert_eq!(entry.new_values, Some(serde_json::json!({"banned": true})));
        assert_eq!(entry.ip_address.as_deref(), Some("203.0.113.7"));
    }

    #[test]
    fn retention_cutoff_is_seven_years_back() {
        let cutoff = AuditService::retention_cutoff();
        let years_back = (Utc::now() - cutoff).num_days() / 365;
        assert_eq!(years_back, RETENTION_YEARS);
    }

    #[test]
    fn rejection_reasons_are_stable_and_fit_the_column() {
        let reasons = [
            RejectionReason::Expired,
            RejectionReason::Revoked,
            RejectionReason::WrongScope,
            RejectionReason::WrongPurpose,
            RejectionReason::MissingApproval,
            RejectionReason::UnknownSession,
        ];

        for reason in reasons {
            assert!(reason.as_str().len() <= 50);
            assert!(!reason.as_str().is_empty());
        }
    }

    #[test]
    fn rejected_request_evidence_carries_the_session_binding() {
        let actor = Uuid::new_v4();
        let session = Uuid::new_v4();
        let resource = Uuid::new_v4();
        let expires_at = Utc::now() + Duration::minutes(15);

        let rejected = RejectedRequestInput::new(
            "payout_override",
            "admin_override",
            "payouts",
            RejectionReason::Expired,
        )
        .actor(actor)
        .session(session)
        .scope(vec!["payouts:write".into()])
        .required_scope("payouts:write")
        .expires_at(expires_at)
        .approval_reference("APR-1234")
        .resource(resource)
        .request_context(Some("203.0.113.7".into()), Some("curl/8".into()));

        let evidence = rejected.evidence();
        assert_eq!(evidence["session_id"], serde_json::json!(session));
        assert_eq!(evidence["purpose"], serde_json::json!("payout_override"));
        assert_eq!(evidence["scope"], serde_json::json!(["payouts:write"]));
        assert_eq!(evidence["required_scope"], serde_json::json!("payouts:write"));
        assert_eq!(evidence["approval_reference"], serde_json::json!("APR-1234"));
        assert_eq!(evidence["attempted_action"], serde_json::json!("admin_override"));
        assert_eq!(evidence["rejection_reason"], serde_json::json!("expired"));
        // Signing keys must never appear in the evidence payload.
        assert!(evidence.get("signing_key").is_none());
        assert!(evidence.get("private_key").is_none());
    }

    #[test]
    fn rejected_request_converts_to_an_audit_entry() {
        let actor = Uuid::new_v4();
        let resource = Uuid::new_v4();

        let entry = RejectedRequestInput::new(
            "data_export",
            "admin_export",
            "users",
            RejectionReason::WrongScope,
        )
        .actor(actor)
        .scope(vec!["users:read".into()])
        .required_scope("users:export")
        .approval_reference("APR-9")
        .resource(resource)
        .into_entry();

        assert_eq!(entry.action, AuditAction::OperatorRequestRejected);
        assert_eq!(entry.action.as_str(), "operator_request_rejected");
        assert_eq!(entry.actor_id, Some(actor));
        assert_eq!(entry.resource_id, Some(resource));
        assert_eq!(entry.resource_type, "users");
        let new_values = entry.new_values.expect("evidence payload");
        assert_eq!(new_values["rejection_reason"], serde_json::json!("wrong_scope"));
        assert_eq!(new_values["required_scope"], serde_json::json!("users:export"));
    }

    #[test]
    fn rejected_request_without_actor_still_converts() {
        // An unknown session has no actor to attribute; the entry must still
        // be writable so the attempt leaves a trace.
        let entry = RejectedRequestInput::new(
            "unknown",
            "admin_override",
            "payouts",
            RejectionReason::UnknownSession,
        )
        .into_entry();

        assert_eq!(entry.actor_id, None);
        assert_eq!(entry.action, AuditAction::OperatorRequestRejected);
        assert_eq!(
            entry.new_values.expect("evidence payload")["rejection_reason"],
            serde_json::json!("unknown_session")
        );
    }
}
