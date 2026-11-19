use crate::api_error::ApiError;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct DatasetContractInput {
    pub dataset_name: String,
    pub owner: String,
    pub schema: Value,
    pub approved_purpose: String,
    pub access_tier: String,
    pub retention_days: i32,
    pub deidentification_rule: String,
}

#[derive(Debug, Deserialize)]
pub struct QualityEventInput {
    pub source_event_id: Uuid,
    pub dataset_name: String,
    pub event_type: String,
    pub event_at: DateTime<Utc>,
    pub payload: Value,
}

#[derive(Debug, Deserialize)]
pub struct QualityThresholdInput {
    pub dataset_name: String,
    pub event_type: String,
    pub window_seconds: i32,
    pub min_events: i32,
    pub max_events: Option<i32>,
    pub max_null_rate: f64,
    pub max_age_seconds: i32,
    pub range_constraints: Value,
    pub severity: String,
}

#[derive(Debug, Deserialize)]
pub struct RetentionExceptionInput {
    pub dataset_name: String,
    pub scope: Value,
    pub reason: String,
    pub duration_hours: i32,
}

#[derive(Debug, Serialize)]
pub struct QualityEventResult {
    pub id: Uuid,
    pub status: String,
    pub checks: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct RetentionRunResult {
    pub id: Uuid,
    pub status: String,
    pub dry_run: bool,
    pub examined_count: i64,
    pub deleted_count: i64,
    pub watermark: Option<DateTime<Utc>>,
    pub retryable: bool,
}

#[derive(Clone)]
pub struct AnalyticsGovernanceService {
    db: PgPool,
}

impl AnalyticsGovernanceService {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }

    pub async fn put_contract(
        &self,
        input: &DatasetContractInput,
        actor_id: Uuid,
    ) -> Result<(), ApiError> {
        let valid_name = !input.dataset_name.is_empty()
            && input.dataset_name.len() <= 100
            && input
                .dataset_name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'));
        if !valid_name
            || input.owner.trim().is_empty()
            || input.approved_purpose.trim().is_empty()
            || input.deidentification_rule.trim().is_empty()
            || input.retention_days <= 0
            || !matches!(
                input.access_tier.as_str(),
                "public" | "internal" | "confidential" | "restricted"
            )
        {
            return Err(ApiError::bad_request("Invalid analytics dataset contract"));
        }

        let schema_object = input
            .schema
            .as_object()
            .ok_or_else(|| ApiError::bad_request("Dataset schema must be a JSON Schema object"))?;
        if schema_object.get("type").and_then(Value::as_str) != Some("object") {
            return Err(ApiError::bad_request("Dataset schema must describe an object"));
        }
        validate_field_schema(&input.schema, false)?;
        jsonschema::draft7::options()
            .build(&input.schema)
            .map_err(|error| ApiError::bad_request(format!("Invalid dataset schema: {error}")))?;

        let mut tx = self.db.begin().await.map_err(ApiError::database_error)?;
        sqlx::query(
            r#"
            INSERT INTO analytics_dataset_contracts
                (dataset_name, owner, schema_json, approved_purpose, access_tier,
                 retention_days, deidentification_rule, status, updated_by, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, 'active', $8, NOW())
            ON CONFLICT (dataset_name) DO UPDATE SET
                owner = EXCLUDED.owner,
                schema_json = EXCLUDED.schema_json,
                approved_purpose = EXCLUDED.approved_purpose,
                access_tier = EXCLUDED.access_tier,
                retention_days = EXCLUDED.retention_days,
                deidentification_rule = EXCLUDED.deidentification_rule,
                status = 'active',
                updated_by = EXCLUDED.updated_by,
                updated_at = NOW()
            "#,
        )
        .bind(&input.dataset_name)
        .bind(input.owner.trim())
        .bind(&input.schema)
        .bind(input.approved_purpose.trim())
        .bind(&input.access_tier)
        .bind(input.retention_days)
        .bind(input.deidentification_rule.trim())
        .bind(actor_id)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::database_error)?;
        sqlx::query(
            r#"
            INSERT INTO analytics_retention_policies
                (dataset_name, owner, target_table, retention_days, eligibility_rule)
            VALUES ($1, $2, 'analytics_quality_events', $3,
                    'received_at <= now() - retention_days; active reviewed exceptions are excluded')
            ON CONFLICT (dataset_name) DO UPDATE SET
                owner = EXCLUDED.owner,
                retention_days = EXCLUDED.retention_days,
                eligibility_rule = EXCLUDED.eligibility_rule,
                enabled = TRUE,
                updated_at = NOW()
            "#,
        )
        .bind(&input.dataset_name)
        .bind(input.owner.trim())
        .bind(input.retention_days)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::database_error)?;
        tx.commit().await.map_err(ApiError::database_error)
    }

    pub async fn list_contracts(&self) -> Result<Vec<Value>, ApiError> {
        sqlx::query_scalar(
            r#"
            SELECT jsonb_build_object(
                'dataset_name', dataset_name,
                'owner', owner,
                'schema', schema_json,
                'approved_purpose', approved_purpose,
                'access_tier', access_tier,
                'retention_days', retention_days,
                'deidentification_rule', deidentification_rule,
                'status', status,
                'updated_at', updated_at
            )
            FROM analytics_dataset_contracts
            ORDER BY dataset_name
            "#,
        )
        .fetch_all(&self.db)
        .await
        .map_err(ApiError::database_error)
    }

    pub async fn grant_access(
        &self,
        dataset_name: &str,
        user_id: Uuid,
        actor_id: Uuid,
        review_days: i32,
    ) -> Result<Uuid, ApiError> {
        if !(1..=365).contains(&review_days) {
            return Err(ApiError::bad_request("Access review must be due within 1-365 days"));
        }
        let grant_id: Uuid = sqlx::query_scalar(
            r#"
            INSERT INTO analytics_dataset_access_grants
                (dataset_name, user_id, granted_by, review_due_at)
            SELECT $1, $2, $3, NOW() + make_interval(days => $4)
            WHERE EXISTS (
                SELECT 1 FROM analytics_dataset_contracts
                WHERE dataset_name = $1 AND status = 'active'
                  AND access_tier <> 'restricted'
            )
            RETURNING id
            "#,
        )
        .bind(dataset_name)
        .bind(user_id)
        .bind(actor_id)
        .bind(review_days)
        .fetch_optional(&self.db)
        .await
        .map_err(ApiError::database_error)?
        .ok_or_else(|| ApiError::not_found("Active dataset contract not found"))?;
        Ok(grant_id)
    }

    pub async fn revoke_access(
        &self,
        dataset_name: &str,
        user_id: Uuid,
        actor_id: Uuid,
    ) -> Result<(), ApiError> {
        let result = sqlx::query(
            r#"
            UPDATE analytics_dataset_access_grants
            SET revoked_at = NOW(), revoked_by = $3
            WHERE dataset_name = $1 AND user_id = $2 AND revoked_at IS NULL
            "#,
        )
        .bind(dataset_name)
        .bind(user_id)
        .bind(actor_id)
        .execute(&self.db)
        .await
        .map_err(ApiError::database_error)?;
        if result.rows_affected() == 0 {
            return Err(ApiError::not_found("Active dataset access grant not found"));
        }
        Ok(())
    }

    pub async fn list_access_grants(&self) -> Result<Vec<Value>, ApiError> {
        sqlx::query_scalar(
            r#"
            SELECT jsonb_build_object(
                'id', id,
                'dataset_name', dataset_name,
                'user_id', user_id,
                'granted_by', granted_by,
                'granted_at', granted_at,
                'review_due_at', review_due_at,
                'revoked_at', revoked_at,
                'revoked_by', revoked_by
            )
            FROM analytics_dataset_access_grants
            ORDER BY granted_at DESC
            "#,
        )
        .fetch_all(&self.db)
        .await
        .map_err(ApiError::database_error)
    }

    pub async fn put_threshold(
        &self,
        input: &QualityThresholdInput,
    ) -> Result<(), ApiError> {
        if input.dataset_name.is_empty()
            || input.event_type.trim().is_empty()
            || input.window_seconds <= 0
            || input.min_events < 0
            || input.max_events.is_some_and(|max| max < input.min_events)
            || !(0.0..=1.0).contains(&input.max_null_rate)
            || input.max_age_seconds <= 0
            || !input.range_constraints.is_object()
            || !matches!(input.severity.as_str(), "info" | "warning" | "critical")
        {
            return Err(ApiError::bad_request("Invalid per-event quality threshold"));
        }
        if let Some(constraints) = input.range_constraints.as_object() {
            for constraint in constraints.values() {
                let bounds = constraint
                    .as_object()
                    .ok_or_else(|| ApiError::bad_request("Range constraints must be objects"))?;
                let min = bounds.get("min").and_then(Value::as_f64);
                let max = bounds.get("max").and_then(Value::as_f64);
                if (min.is_none() && max.is_none())
                    || min.zip(max).is_some_and(|(minimum, maximum)| minimum > maximum)
                {
                    return Err(ApiError::bad_request("Invalid range constraint bounds"));
                }
            }
        }
        let result = sqlx::query(
            r#"
            INSERT INTO analytics_quality_thresholds
                (dataset_name, event_type, window_seconds, min_events, max_events,
                 max_null_rate, max_age_seconds, range_constraints, severity)
            SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9
            WHERE EXISTS (
                SELECT 1 FROM analytics_dataset_contracts
                WHERE dataset_name = $1 AND status = 'active'
            )
            ON CONFLICT (dataset_name, event_type) DO UPDATE SET
                window_seconds = EXCLUDED.window_seconds,
                min_events = EXCLUDED.min_events,
                max_events = EXCLUDED.max_events,
                max_null_rate = EXCLUDED.max_null_rate,
                max_age_seconds = EXCLUDED.max_age_seconds,
                range_constraints = EXCLUDED.range_constraints,
                severity = EXCLUDED.severity
            "#,
        )
        .bind(&input.dataset_name)
        .bind(input.event_type.trim())
        .bind(input.window_seconds)
        .bind(input.min_events)
        .bind(input.max_events)
        .bind(input.max_null_rate)
        .bind(input.max_age_seconds)
        .bind(&input.range_constraints)
        .bind(&input.severity)
        .execute(&self.db)
        .await
        .map_err(ApiError::database_error)?;
        if result.rows_affected() == 0 {
            return Err(ApiError::not_found("Active dataset contract not found"));
        }
        Ok(())
    }

    pub async fn publish_event(
        &self,
        event: &QualityEventInput,
        actor_id: Uuid,
        is_admin: bool,
    ) -> Result<QualityEventResult, ApiError> {
        let mut tx = self.db.begin().await.map_err(ApiError::database_error)?;
        let contract: Option<(Value, String)> = sqlx::query_as(
            "SELECT schema_json, status FROM analytics_dataset_contracts WHERE dataset_name = $1",
        )
        .bind(&event.dataset_name)
        .fetch_optional(&mut *tx)
        .await
        .map_err(ApiError::database_error)?;
        let (schema, contract_status) =
            contract.ok_or_else(|| ApiError::not_found("Dataset contract not found"))?;
        if contract_status != "active" {
            return Err(ApiError::forbidden("Dataset publication is suspended"));
        }
        if !is_admin {
            let has_access: bool = sqlx::query_scalar(
                r#"
                SELECT EXISTS (
                    SELECT 1 FROM analytics_dataset_access_grants
                    WHERE dataset_name = $1 AND user_id = $2 AND revoked_at IS NULL
                      AND review_due_at > NOW()
                )
                "#,
            )
            .bind(&event.dataset_name)
            .bind(actor_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(ApiError::database_error)?;
            if !has_access {
                return Err(ApiError::forbidden("Active dataset access grant required"));
            }
        }

        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1), hashtext($2))")
            .bind(&event.dataset_name)
            .bind(event.source_event_id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(ApiError::database_error)?;
        let schema_validator = jsonschema::draft7::options()
            .build(&schema)
            .map_err(|error| ApiError::internal_error(format!("Stored dataset schema is invalid: {error}")))?;
        let mut checks = Vec::new();
        if !schema_validator.is_valid(&event.payload) {
            checks.push("contract_schema".to_string());
        }
        let threshold: Option<(i32, i32, Option<i32>, f64, i32, Value, String)> =
            sqlx::query_as(
                r#"
                SELECT window_seconds, min_events, max_events, max_null_rate::FLOAT8,
                       max_age_seconds, range_constraints, severity
                FROM analytics_quality_thresholds
                WHERE dataset_name = $1 AND event_type = $2
                "#,
            )
            .bind(&event.dataset_name)
            .bind(&event.event_type)
            .fetch_optional(&mut *tx)
            .await
            .map_err(ApiError::database_error)?;
        let (window_seconds, min_events, max_events, max_null_rate, max_age_seconds, ranges, severity) =
            threshold.ok_or_else(|| ApiError::bad_request("Quality thresholds are not configured"))?;

        let duplicate: bool = sqlx::query_scalar(
            r#"
            SELECT EXISTS (
                SELECT 1 FROM analytics_quality_events
                WHERE dataset_name = $1 AND source_event_id = $2
                  AND status <> 'quarantined'
            )
            "#,
        )
        .bind(&event.dataset_name)
        .bind(event.source_event_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(ApiError::database_error)?;
        if duplicate {
            checks.push("duplicate_event".to_string());
        }

        let now = Utc::now();
        let age_seconds = (now - event.event_at).num_seconds();
        if age_seconds < 0 || age_seconds > i64::from(max_age_seconds) {
            checks.push("freshness".to_string());
        }

        let fields = event.payload.as_object();
        let null_rate = fields
            .filter(|fields| !fields.is_empty())
            .map(|fields| {
                fields.values().filter(|value| value.is_null()).count() as f64
                    / fields.len() as f64
            })
            .unwrap_or(0.0);
        if null_rate > max_null_rate {
            checks.push("null_rate".to_string());
        }

        if let (Some(payload), Some(ranges)) = (fields, ranges.as_object()) {
            for (field, constraint) in ranges {
                let Some(value) = payload.get(field).and_then(Value::as_f64) else {
                    if payload.contains_key(field) {
                        checks.push(format!("range:{field}"));
                    }
                    continue;
                };
                let min = constraint.get("min").and_then(Value::as_f64);
                let max = constraint.get("max").and_then(Value::as_f64);
                if min.is_some_and(|minimum| value < minimum)
                    || max.is_some_and(|maximum| value > maximum)
                {
                    checks.push(format!("range:{field}"));
                }
            }
        }

        let recent_count: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*) FROM analytics_quality_events
            WHERE dataset_name = $1 AND event_type = $2
              AND received_at >= NOW() - make_interval(secs => $3)
              AND status <> 'quarantined'
            "#,
        )
        .bind(&event.dataset_name)
        .bind(&event.event_type)
        .bind(window_seconds)
        .fetch_one(&mut *tx)
        .await
        .map_err(ApiError::database_error)?;
        let projected_count = recent_count + 1;
        if projected_count < i64::from(min_events)
            || max_events.is_some_and(|limit| projected_count > i64::from(limit))
        {
            checks.push("volume".to_string());
        }

        let quarantine = checks.iter().any(|check| {
            check == "contract_schema"
                || check == "duplicate_event"
                || check == "null_rate"
                || check.starts_with("range:")
        });
        let status = if quarantine {
            "quarantined"
        } else if !checks.is_empty() {
            "degraded"
        } else {
            "healthy"
        };
        let persisted_payload = if quarantine {
            serde_json::json!({})
        } else {
            event.payload.clone()
        };
        let event_row_id = Uuid::new_v4();
        sqlx::query(
            r#"
            INSERT INTO analytics_quality_events
                (id, source_event_id, dataset_name, event_type, event_at, payload, status, check_results)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            "#,
        )
        .bind(event_row_id)
        .bind(event.source_event_id)
        .bind(&event.dataset_name)
        .bind(&event.event_type)
        .bind(event.event_at)
        .bind(persisted_payload)
        .bind(status)
        .bind(
            serde_json::to_value(&checks)
                .map_err(|error| ApiError::internal_error(error.to_string()))?,
        )
        .execute(&mut *tx)
        .await
        .map_err(ApiError::database_error)?;
        tx.commit().await.map_err(ApiError::database_error)?;

        if !checks.is_empty() {
            match severity.as_str() {
                "critical" => tracing::error!(
                    dataset = %event.dataset_name,
                    event_type = %event.event_type,
                    status,
                    checks = ?checks,
                    "Analytics data quality check failed; operator escalation required"
                ),
                "warning" => tracing::warn!(
                    dataset = %event.dataset_name,
                    event_type = %event.event_type,
                    status,
                    checks = ?checks,
                    "Analytics data quality check failed"
                ),
                _ => tracing::info!(
                    dataset = %event.dataset_name,
                    event_type = %event.event_type,
                    status,
                    checks = ?checks,
                    "Analytics data quality check failed"
                ),
            }
        }
        Ok(QualityEventResult {
            id: event_row_id,
            status: status.to_string(),
            checks,
        })
    }

    pub async fn quality_summary(
        &self,
        dataset_name: &str,
        event_type: &str,
        actor_id: Uuid,
        is_admin: bool,
    ) -> Result<Value, ApiError> {
        if !is_admin {
            let has_access: bool = sqlx::query_scalar(
                r#"
                SELECT EXISTS (
                    SELECT 1 FROM analytics_dataset_access_grants
                    WHERE dataset_name = $1 AND user_id = $2 AND revoked_at IS NULL
                      AND review_due_at > NOW()
                )
                "#,
            )
            .bind(dataset_name)
            .bind(actor_id)
            .fetch_one(&self.db)
            .await
            .map_err(ApiError::database_error)?;
            if !has_access {
                return Err(ApiError::forbidden("Active dataset access grant required"));
            }
        }
        sqlx::query_scalar(
            r#"
            SELECT jsonb_build_object(
                'dataset_name', $1,
                'event_type', $2,
                'quality_status', COALESCE(
                    (SELECT CASE WHEN status = 'healthy' THEN 'healthy' ELSE 'degraded' END
                     FROM analytics_quality_events
                     WHERE dataset_name = $1 AND event_type = $2
                     ORDER BY received_at DESC, id DESC
                     LIMIT 1),
                    'unknown'
                ),
                'accepted_events', COUNT(*) FILTER (WHERE status <> 'quarantined'),
                'quarantined_events', COUNT(*) FILTER (WHERE status = 'quarantined'),
                'last_received_at', MAX(received_at)
            )
            FROM analytics_quality_events
            WHERE dataset_name = $1 AND event_type = $2
            "#,
        )
        .bind(dataset_name)
        .bind(event_type)
        .fetch_one(&self.db)
        .await
        .map_err(ApiError::database_error)
    }

    pub async fn run_retention_purge(
        &self,
        dataset_name: &str,
        dry_run: bool,
        batch_limit: i64,
    ) -> Result<RetentionRunResult, ApiError> {
        if !(1..=10_000).contains(&batch_limit) {
            return Err(ApiError::bad_request("batch_limit must be between 1 and 10000"));
        }
        let policy: Option<(Uuid, bool)> = sqlx::query_as(
            "SELECT id, enabled FROM analytics_retention_policies WHERE dataset_name = $1",
        )
        .bind(dataset_name)
        .fetch_optional(&self.db)
        .await
        .map_err(ApiError::database_error)?;
        let (policy_id, enabled) =
            policy.ok_or_else(|| ApiError::not_found("Retention policy not found"))?;
        if !enabled {
            return Err(ApiError::conflict("Retention policy is disabled"));
        }

        let run_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO analytics_retention_runs (id, policy_id, dry_run, status) VALUES ($1, $2, $3, 'running')",
        )
        .bind(run_id)
        .bind(policy_id)
        .bind(dry_run)
        .execute(&self.db)
        .await
        .map_err(ApiError::database_error)?;

        let result = async {
            let mut tx = self.db.begin().await.map_err(ApiError::database_error)?;
            let policy: Option<(i32, String)> = sqlx::query_as(
                r#"
                SELECT retention_days, target_table
                FROM analytics_retention_policies
                WHERE id = $1 AND enabled = TRUE
                FOR UPDATE
                "#,
            )
            .bind(policy_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(ApiError::database_error)?;
            let (retention_days, target_table) =
                policy.ok_or_else(|| ApiError::conflict("Retention policy is disabled"))?;
            if target_table != "analytics_quality_events" {
                return Err(ApiError::internal_error("Unsupported retention target"));
            }

            let eligibility = r#"
                dataset_name = $1
                AND received_at < NOW() - make_interval(days => $2)
                AND NOT EXISTS (
                    SELECT 1 FROM analytics_retention_exceptions exception
                    WHERE exception.policy_id = $3
                      AND exception.reviewed_at IS NOT NULL
                      AND exception.approved = TRUE
                      AND exception.starts_at <= NOW()
                      AND exception.expires_at > NOW()
                      AND (
                        exception.scope = '{}'::jsonb
                        OR (
                            exception.scope->>'dataset_name' = $1
                            AND (
                                exception.scope->>'event_type' IS NULL
                                OR exception.scope->>'event_type' = analytics_quality_events.event_type
                            )
                        )
                      )
                )
            "#;
            let count_query =
                format!("SELECT COUNT(*) FROM analytics_quality_events WHERE {eligibility}");
            let examined: i64 = sqlx::query_scalar(&count_query)
                .bind(dataset_name)
                .bind(retention_days)
                .bind(policy_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(ApiError::database_error)?;

            let mut deleted = 0i64;
            let mut watermark: Option<DateTime<Utc>> = None;
            if !dry_run {
                let purge_query = format!(
                    r#"
                    WITH eligible AS (
                        SELECT id FROM analytics_quality_events
                        WHERE {eligibility}
                        ORDER BY received_at, id
                        LIMIT $4
                        FOR UPDATE SKIP LOCKED
                    ),
                    removed AS (
                        DELETE FROM analytics_quality_events event
                        USING eligible
                        WHERE event.id = eligible.id
                        RETURNING event.received_at
                    )
                    SELECT COUNT(*)::BIGINT, MAX(received_at)
                    FROM removed
                    "#
                );
                let purge_result: (i64, Option<DateTime<Utc>>) =
                    sqlx::query_as(&purge_query)
                        .bind(dataset_name)
                        .bind(retention_days)
                        .bind(policy_id)
                        .bind(batch_limit)
                        .fetch_one(&mut *tx)
                        .await
                        .map_err(ApiError::database_error)?;
                deleted = purge_result.0;
                if let Some(event_watermark) = purge_result.1 {
                    watermark = Some(
                        watermark.map_or(event_watermark, |current| current.max(event_watermark)),
                    );
                }
                watermark = watermark.or(Some(
                    Utc::now() - Duration::days(i64::from(retention_days)),
                ));
                sqlx::query(
                    r#"
                    UPDATE analytics_retention_policies
                    SET last_success_at = NOW(), last_successful_watermark = $2, updated_at = NOW()
                    WHERE id = $1
                    "#,
                )
                .bind(policy_id)
                .bind(watermark)
                .execute(&mut *tx)
                .await
                .map_err(ApiError::database_error)?;
            }
            sqlx::query(
                r#"
                UPDATE analytics_retention_runs
                SET status = 'succeeded', completed_at = NOW(), examined_count = $2,
                    deleted_count = $3, watermark = $4
                WHERE id = $1
                "#,
            )
            .bind(run_id)
            .bind(examined)
            .bind(deleted)
            .bind(watermark)
            .execute(&mut *tx)
            .await
            .map_err(ApiError::database_error)?;
            tx.commit().await.map_err(ApiError::database_error)?;

            Ok(RetentionRunResult {
                id: run_id,
                status: "succeeded".to_string(),
                dry_run,
                examined_count: examined,
                deleted_count: deleted,
                watermark,
                retryable: false,
            })
        }
        .await;

        if let Err(error) = result {
            sqlx::query(
                r#"
                UPDATE analytics_retention_runs
                SET status = 'failed', completed_at = NOW(), retryable = TRUE,
                    error_code = 'purge_failed'
                WHERE id = $1
                "#,
            )
            .bind(run_id)
            .execute(&self.db)
            .await
            .map_err(ApiError::database_error)?;
            tracing::error!(
                dataset = %dataset_name,
                run_id = %run_id,
                error = %error,
                "Analytics retention purge failed; run is retryable"
            );
            return Err(error);
        }
        result
    }

    pub async fn run_due_purges(&self) -> Result<(), ApiError> {
        let datasets: Vec<String> = sqlx::query_scalar(
            r#"
            SELECT dataset_name
            FROM analytics_retention_policies
            WHERE enabled = TRUE
              AND (last_success_at IS NULL OR last_success_at < NOW() - INTERVAL '1 hour')
            ORDER BY dataset_name
            "#,
        )
        .fetch_all(&self.db)
        .await
        .map_err(ApiError::database_error)?;
        let mut failed = false;
        for dataset_name in datasets {
            if let Err(error) = self.run_retention_purge(&dataset_name, false, 10_000).await {
                failed = true;
                tracing::error!(
                    dataset = %dataset_name,
                    error = %error,
                    "Scheduled analytics retention purge failed"
                );
            }
        }
        if failed {
            return Err(ApiError::ServiceUnavailable(
                "One or more analytics retention purges failed; consult the run ledger".to_string(),
            ));
        }
        Ok(())
    }

    pub async fn create_retention_exception(
        &self,
        input: &RetentionExceptionInput,
        requester: Uuid,
    ) -> Result<Uuid, ApiError> {
        if input.reason.trim().is_empty()
            || input.duration_hours <= 0
            || input.duration_hours > 720
            || !input.scope.is_object()
        {
            return Err(ApiError::bad_request(
                "Retention exception needs a reason, object scope, and duration up to 30 days",
            ));
        }
        if let Some(scope) = input.scope.as_object() {
            if scope.keys().any(|key| !matches!(key.as_str(), "dataset_name" | "event_type")) {
                return Err(ApiError::bad_request("Unsupported retention exception scope"));
            }
            if !scope.is_empty()
                && scope.get("dataset_name").and_then(Value::as_str)
                    != Some(input.dataset_name.as_str())
            {
                return Err(ApiError::bad_request(
                    "Scoped exceptions must name the policy dataset",
                ));
            }
            if scope.get("event_type").is_some_and(|event_type| {
                !event_type
                    .as_str()
                    .is_some_and(|value| !value.trim().is_empty())
            }) {
                return Err(ApiError::bad_request("event_type scope must be a non-empty string"));
            }
        }

        let id: Option<Uuid> = sqlx::query_scalar(
            r#"
            INSERT INTO analytics_retention_exceptions
                (policy_id, scope, reason, expires_at, requested_by)
            SELECT id, $2, $3, NOW() + make_interval(hours => $4), $5
            FROM analytics_retention_policies
            WHERE dataset_name = $1 AND enabled = TRUE
            RETURNING id
            "#,
        )
        .bind(&input.dataset_name)
        .bind(&input.scope)
        .bind(input.reason.trim())
        .bind(input.duration_hours)
        .bind(requester)
        .fetch_optional(&self.db)
        .await
        .map_err(ApiError::database_error)?;
        id.ok_or_else(|| ApiError::not_found("Enabled dataset retention policy not found"))
    }

    pub async fn review_retention_exception(
        &self,
        exception_id: Uuid,
        reviewer: Uuid,
        approve: bool,
    ) -> Result<(), ApiError> {
        let result = sqlx::query(
            r#"
            UPDATE analytics_retention_exceptions
            SET reviewed_by = $2, reviewed_at = NOW(), approved = $3
            WHERE id = $1 AND reviewed_at IS NULL
              AND requested_by <> $2 AND expires_at > NOW()
            "#,
        )
        .bind(exception_id)
        .bind(reviewer)
        .bind(approve)
        .execute(&self.db)
        .await
        .map_err(ApiError::database_error)?;
        if result.rows_affected() == 0 {
            return Err(ApiError::conflict(
                "Exception is expired, already reviewed, or requires a different reviewer",
            ));
        }
        Ok(())
    }

    pub async fn list_retention_runs(&self) -> Result<Vec<Value>, ApiError> {
        sqlx::query_scalar(
            r#"
            SELECT jsonb_build_object(
                'id', run.id,
                'dataset_name', policy.dataset_name,
                'started_at', run.started_at,
                'completed_at', run.completed_at,
                'dry_run', run.dry_run,
                'status', run.status,
                'examined_count', run.examined_count,
                'deleted_count', run.deleted_count,
                'watermark', run.watermark,
                'retryable', run.retryable,
                'error_code', run.error_code,
                'last_success_at', policy.last_success_at,
                'last_successful_watermark', policy.last_successful_watermark
            )
            FROM analytics_retention_runs run
            JOIN analytics_retention_policies policy ON policy.id = run.policy_id
            ORDER BY run.started_at DESC
            LIMIT 200
            "#,
        )
        .fetch_all(&self.db)
        .await
        .map_err(ApiError::database_error)
    }
}

fn validate_field_schema(schema: &Value, is_field: bool) -> Result<(), ApiError> {
    let schema = schema
        .as_object()
        .ok_or_else(|| ApiError::bad_request("Schema fields must be JSON Schema objects"))?;
    if schema.contains_key("$ref") {
        return Err(ApiError::bad_request(
            "Dataset schemas cannot use $ref for field classification",
        ));
    }
    if is_field
        && !matches!(
            schema
                .get("x-data-classification")
                .and_then(Value::as_str),
            Some("public" | "internal" | "confidential")
        )
    {
        return Err(ApiError::bad_request(
            "Every dataset field needs an approved non-restricted x-data-classification",
        ));
    }

    match schema.get("type").and_then(Value::as_str) {
        Some("object") => {
            if schema.get("additionalProperties") != Some(&Value::Bool(false)) {
                return Err(ApiError::bad_request(
                    "Every object schema must set additionalProperties=false",
                ));
            }
            let properties = schema
                .get("properties")
                .and_then(Value::as_object)
                .ok_or_else(|| ApiError::bad_request("Object schemas must declare properties"))?;
            for property in properties.values() {
                validate_field_schema(property, true)?;
            }
        }
        Some("array") => {
            let items = schema
                .get("items")
                .ok_or_else(|| ApiError::bad_request("Array schemas must declare items"))?;
            validate_field_schema(items, false)?;
        }
        _ => {}
    }
    Ok(())
}
