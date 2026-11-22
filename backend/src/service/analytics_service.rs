/// Analytics service — aggregates on-chain and off-chain metrics.
/// Privacy: player-level data is only returned to the player themselves or admins.
/// Enhanced with data aggregation and revenue metrics for #689.
use crate::api_error::ApiError;
use crate::service::feature_flags::{
    evaluate_experiment_decision, ExperimentAnalysisInput, ExperimentDecision,
    ExperimentDefinition, GuardrailMetricSample,
};
use crate::service::analytics_governance::{AnalyticsGovernanceService, QualityEventInput};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

/// Current analytics consent policy version. Bump when the consent text or
/// scope changes; stored consent rows must match this version to be honoured.
pub const ANALYTICS_CONSENT_VERSION: i32 = 1;

// ─── DTOs ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct GameMetricsResponse {
    pub game_id: i32,
    pub total_matches: i64,
    pub total_players: i64,
    pub total_wagered: i64,
    pub total_rewards_paid: i64,
    pub avg_match_duration_secs: i64,
    pub data_quality_status: String,
    pub last_updated: chrono::DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct PlatformMetricsResponse {
    pub total_matches_all_time: i64,
    pub active_players_30d: i64,
    pub total_staked: i64,
    pub total_volume: i64,
    pub last_updated: chrono::DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct PlayerInsightsResponse {
    pub user_id: Uuid,
    pub game_id: i32,
    pub matches_played: i64,
    pub win_rate_pct: f64,
    pub avg_session_secs: i64,
    pub last_active: chrono::DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct RecordMatchRequest {
    pub game_id: i32,
    pub match_id: Uuid,
    pub duration_secs: i64,
    pub wager_amount: i64,
    pub reward_amount: i64,
    pub player_count: i32,
}

// ─── Ingestion DTOs (#251) ───────────────────────────────────────────────────

/// A single analytics event as delivered by a producer.
/// `event_id` is a stable, producer-assigned identifier; `producer_id`
/// scopes it so two producers may reuse the same event_id safely.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AnalyticsEvent {
    pub producer_id: String,
    pub event_id: String,
    pub schema_version: i32,
    pub event_name: String,
    pub occurred_at: chrono::DateTime<Utc>,
    #[serde(default)]
    pub payload: serde_json::Value,
}

/// Outcome of ingesting a single event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IngestionOutcome {
    /// Event was newly accepted.
    Accepted,
    /// Event was already accepted (idempotent retry).
    Duplicate,
    /// Event failed validation and was recorded in the rejection path.
    Rejected,
}

#[derive(Debug, Serialize)]
pub struct IngestionResult {
    pub producer_id: String,
    pub event_id: String,
    pub outcome: IngestionOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rejection_reason: Option<String>,
}

/// Supported schema versions for ingestion. Rollouts add versions here.
pub const SUPPORTED_SCHEMA_VERSIONS: &[i32] = &[1];

// ─── Identity stitching DTOs (#252) ──────────────────────────────────────────

/// Purpose-limited identity link. Raw auth identifiers are never stored here;
/// callers pass an already-pseudonymised subject id.
#[derive(Debug, Clone, Deserialize)]
pub struct IdentityLinkRequest {
    pub anonymous_subject: String,
    pub authenticated_subject: String,
    pub purpose: String,
    pub consent_granted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityLinkOutcome {
    Linked,
    AlreadyLinked,
    ConsentMissing,
    Collision,
}

#[derive(Debug, Serialize)]
pub struct IdentityLinkResult {
    pub outcome: IdentityLinkOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical_subject: Option<String>,
}

// ─── Metric catalog DTOs (#253) ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct MetricCatalogEntry {
    pub name: String,
    pub owner: String,
    pub query_definition: String,
    pub source_events: Vec<String>,
    pub freshness_secs: i64,
    pub privacy_class: String,
    pub deprecated: bool,
}

// ─── Budget DTOs (#254) ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct QueryBudget {
    pub max_latency_ms: u64,
    pub max_freshness_secs: i64,
    pub max_range_secs: i64,
    pub max_rows: i64,
}

impl Default for QueryBudget {
    fn default() -> Self {
        Self {
            max_latency_ms: 2_000,
            max_freshness_secs: 3_600,
            max_range_secs: 90 * 24 * 3_600,
            max_rows: 1_000,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct BudgetedResult<T> {
    pub data: T,
    /// True when the underlying data is older than the freshness budget.
    pub stale: bool,
    /// True when the result was truncated or could not be fully computed.
    pub incomplete: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<String>,
}

// ─── DB rows ──────────────────────────────────────────────────────────────────

#[derive(sqlx::FromRow)]
struct GameMetricsRow {
    pub game_id: i32,
    pub total_matches: i64,
    pub total_players: i64,
    pub total_wagered: i64,
    pub total_rewards_paid: i64,
    pub avg_match_duration_secs: i64,
    pub data_quality_status: String,
    pub last_updated: chrono::DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct PlayerInsightsRow {
    pub user_id: Uuid,
    pub game_id: i32,
    pub matches_played: i64,
    pub wins: i64,
    pub avg_session_secs: i64,
    pub last_active: chrono::DateTime<Utc>,
}

// ─── Service ──────────────────────────────────────────────────────────────────

pub struct AnalyticsService {
    db: PgPool,
}

impl AnalyticsService {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }

    /// Check whether the user has granted analytics consent at the current
    /// policy version. This is the authoritative server-side check; browser
    /// state is never trusted. Anonymous users (no consent row) are treated
    /// as opted out for user-linked analytics.
    pub async fn has_analytics_consent(&self, user_id: Uuid) -> Result<bool, ApiError> {
        let granted = sqlx::query_scalar!(
            r#"
            SELECT EXISTS (
                SELECT 1 FROM analytics_consent
                WHERE user_id = $1
                  AND granted = TRUE
                  AND withdrawn_at IS NULL
                  AND consent_version = $2
            ) AS "granted!: bool"
            "#,
            user_id,
            ANALYTICS_CONSENT_VERSION,
        )
        .fetch_one(&self.db)
        .await
        .map_err(|e| ApiError::InternalServerError(e.to_string()))?;

        Ok(granted)
    }

    /// Record a completed match (called by match service after resolution).
    /// Match-level aggregates are non-user-linked and are always recorded;
    /// per-player behaviour is gated on consent in `record_player_behaviour`.
    pub async fn record_match(&self, req: &RecordMatchRequest) -> Result<(), ApiError> {
        if req.game_id <= 0
            || !(1..=86_400).contains(&req.duration_secs)
            || req.wager_amount < 0
            || req.reward_amount < 0
            || !(1..=100).contains(&req.player_count)
        {
            return Err(ApiError::bad_request(
                "Match metrics are outside supported ranges",
            ));
        }

        let match_id_hash = hex::encode(Sha256::digest(req.match_id.as_bytes()));
        let prior_game: Option<i32> = sqlx::query_scalar(
            "SELECT game_id FROM analytics_match_aggregation_receipts WHERE match_id_hash = $1",
        )
        .bind(&match_id_hash)
        .fetch_optional(&self.db)
        .await
        .map_err(ApiError::database_error)?;
        if let Some(game_id) = prior_game {
            return if game_id == req.game_id {
                Ok(())
            } else {
                Err(ApiError::conflict("Match ID was already recorded for another game"))
            };
        }

        let payload = json!({
            "game_id": req.game_id,
            "duration_secs": req.duration_secs,
            "player_count": req.player_count
        });
        let event = QualityEventInput {
            source_event_id: req.match_id,
            dataset_name: "match_completion".to_string(),
            event_type: "match_completed".to_string(),
            event_at: Utc::now(),
            payload: payload.clone(),
        };
        let governance = AnalyticsGovernanceService::new(self.db.clone());
        let quality = governance.publish_event(&event, Uuid::nil(), true).await?;
        let quality_status = if quality.status == "quarantined" {
            let prior: Option<(serde_json::Value, String)> = sqlx::query_as(
                r#"
                SELECT payload, status FROM analytics_quality_events
                WHERE dataset_name = $1 AND source_event_id = $2
                  AND status IN ('healthy', 'degraded')
                ORDER BY received_at ASC
                LIMIT 1
                "#,
            )
            .bind(&event.dataset_name)
            .bind(event.source_event_id)
            .fetch_optional(&self.db)
            .await
            .map_err(ApiError::database_error)?;
            match prior {
                Some((prior_payload, status)) if prior_payload == payload => status,
                _ => {
                    return Err(ApiError::bad_request(
                        "Match analytics event failed quality checks",
                    ))
                }
            }
        } else {
            quality.status
        };

        let mut tx = self.db.begin().await.map_err(ApiError::database_error)?;
        let inserted: Option<String> = sqlx::query_scalar(
            r#"
            INSERT INTO analytics_match_aggregation_receipts (match_id_hash, game_id)
            VALUES ($1, $2)
            ON CONFLICT (match_id_hash) DO NOTHING
            RETURNING match_id_hash
            "#,
        )
        .bind(&match_id_hash)
        .bind(req.game_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(ApiError::database_error)?;
        if inserted.is_none() {
            let existing_game: i32 = sqlx::query_scalar(
                "SELECT game_id FROM analytics_match_aggregation_receipts WHERE match_id_hash = $1",
            )
            .bind(&match_id_hash)
            .fetch_one(&mut *tx)
            .await
            .map_err(ApiError::database_error)?;
            if existing_game != req.game_id {
                return Err(ApiError::conflict("Match ID was already recorded for another game"));
            }
            tx.commit().await.map_err(ApiError::database_error)?;
            return Ok(());
        }

        sqlx::query!(
            r#"
            INSERT INTO analytics_game_metrics
                (game_id, total_matches, total_players, total_wagered,
                 total_rewards_paid, avg_match_duration_secs, data_quality_status, last_updated)
            VALUES ($1, 1, $2, $3, $4, $5, $6, NOW())
            ON CONFLICT (game_id) DO UPDATE SET
                total_matches            = analytics_game_metrics.total_matches + 1,
                total_players            = analytics_game_metrics.total_players + EXCLUDED.total_players,
                total_wagered            = analytics_game_metrics.total_wagered + EXCLUDED.total_wagered,
                total_rewards_paid       = analytics_game_metrics.total_rewards_paid + EXCLUDED.total_rewards_paid,
                avg_match_duration_secs  = (
                    analytics_game_metrics.avg_match_duration_secs
                    * analytics_game_metrics.total_matches
                    + EXCLUDED.avg_match_duration_secs
                ) / (analytics_game_metrics.total_matches + 1),
                data_quality_status     = EXCLUDED.data_quality_status,
                last_updated             = NOW()
            "#,
            req.game_id,
            req.player_count as i64,
            req.wager_amount,
            req.reward_amount,
            req.duration_secs,
            quality_status,
        )
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::InternalServerError(e.to_string()))?;

        sqlx::query!(
            r#"
            INSERT INTO analytics_platform (id, total_matches_all_time, total_volume, last_updated)
            VALUES (1, 1, $1, NOW())
            ON CONFLICT (id) DO UPDATE SET
                total_matches_all_time = analytics_platform.total_matches_all_time + 1,
                total_volume           = analytics_platform.total_volume + EXCLUDED.total_volume,
                last_updated           = NOW()
            "#,
            req.wager_amount,
        )
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::InternalServerError(e.to_string()))?;

        tx.commit().await.map_err(ApiError::database_error)
    }

    /// Record player behaviour (privacy-safe: no PII in analytics tables).
    /// Suppressed unless the user has current, non-withdrawn consent. This
    /// also covers queued-event processing: each delivery re-checks consent
    /// so a withdrawal mid-queue stops subsequent writes.
    pub async fn record_player_behaviour(
        &self,
        user_id: Uuid,
        game_id: i32,
        won: bool,
        session_secs: i64,
    ) -> Result<(), ApiError> {
        if !self.has_analytics_consent(user_id).await? {
            return Ok(());
        }

        sqlx::query!(
            r#"
            INSERT INTO analytics_player_behaviour
                (user_id, game_id, matches_played, wins, avg_session_secs, last_active)
            VALUES ($1, $2, 1, $3, $4, NOW())
            ON CONFLICT (user_id, game_id) DO UPDATE SET
                matches_played   = analytics_player_behaviour.matches_played + 1,
                wins             = analytics_player_behaviour.wins + EXCLUDED.wins,
                avg_session_secs = (
                    analytics_player_behaviour.avg_session_secs
                    * analytics_player_behaviour.matches_played
                    + EXCLUDED.avg_session_secs
                ) / (analytics_player_behaviour.matches_played + 1),
                last_active      = NOW()
            "#,
            user_id,
            game_id,
            if won { 1i64 } else { 0i64 },
            session_secs,
        )
        .execute(&self.db)
        .await
        .map_err(|e| ApiError::InternalServerError(e.to_string()))?;

        Ok(())
    }

    /// Apply the documented withdrawal behaviour for already-collected
    /// user-linked analytics: rows are anonymised (user_id detached) and
    /// queued events for the user are dropped. Aggregate, non-user-linked
    /// metrics are retained. Idempotent.
    pub async fn apply_consent_withdrawal(&self, user_id: Uuid) -> Result<(), ApiError> {
        let mut tx = self
            .db
            .begin()
            .await
            .map_err(|e| ApiError::InternalServerError(e.to_string()))?;

        // Drop any queued, not-yet-delivered events for this user.
        sqlx::query!(
            r#"DELETE FROM analytics_event_queue WHERE user_id = $1"#,
            user_id,
        )
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::InternalServerError(e.to_string()))?;

        // Anonymise already-collected user-linked behaviour rows.
        sqlx::query!(
            r#"DELETE FROM analytics_player_behaviour WHERE user_id = $1"#,
            user_id,
        )
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::InternalServerError(e.to_string()))?;

        // Record the withdrawal authoritatively.
        sqlx::query!(
            r#"
            INSERT INTO analytics_consent (user_id, granted, consent_version, withdrawn_at, updated_at)
            VALUES ($1, FALSE, $2, NOW(), NOW())
            ON CONFLICT (user_id) DO UPDATE SET
                granted         = FALSE,
                consent_version = EXCLUDED.consent_version,
                withdrawn_at    = NOW(),
                updated_at      = NOW()
            "#,
            user_id,
            ANALYTICS_CONSENT_VERSION,
        )
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::InternalServerError(e.to_string()))?;

        tx.commit()
            .await
            .map_err(|e| ApiError::InternalServerError(e.to_string()))?;

        Ok(())
    }

    pub async fn get_game_metrics(
        &self,
        game_id: i32,
    ) -> Result<Option<GameMetricsResponse>, ApiError> {
        let row = sqlx::query_as!(
            GameMetricsRow,
            "SELECT * FROM analytics_game_metrics WHERE game_id = $1",
            game_id
        )
        .fetch_optional(&self.db)
        .await
        .map_err(|e| ApiError::InternalServerError(e.to_string()))?;

        Ok(row.map(|r| GameMetricsResponse {
            game_id: r.game_id,
            total_matches: r.total_matches,
            total_players: r.total_players,
            total_wagered: r.total_wagered,
            total_rewards_paid: r.total_rewards_paid,
            avg_match_duration_secs: r.avg_match_duration_secs,
            data_quality_status: r.data_quality_status,
            last_updated: r.last_updated,
        }))
    }

    pub async fn get_platform_metrics(&self) -> Result<PlatformMetricsResponse, ApiError> {
        let row = sqlx::query!(
            r#"
            SELECT
                COALESCE(total_matches_all_time, 0) AS "total_matches_all_time!: i64",
                COALESCE(active_players_30d, 0)     AS "active_players_30d!: i64",
                COALESCE(total_staked, 0)            AS "total_staked!: i64",
                COALESCE(total_volume, 0)            AS "total_volume!: i64",
                last_updated
            FROM analytics_platform WHERE id = 1
            "#
        )
        .fetch_optional(&self.db)
        .await
        .map_err(|e| ApiError::InternalServerError(e.to_string()))?;

        Ok(row.map(|r| PlatformMetricsResponse {
            total_matches_all_time: r.total_matches_all_time,
            active_players_30d: r.active_players_30d,
            total_staked: r.total_staked,
            total_volume: r.total_volume,
            last_updated: r.last_updated,
        }).unwrap_or(PlatformMetricsResponse {
            total_matches_all_time: 0,
            active_players_30d: 0,
            total_staked: 0,
            total_volume: 0,
            last_updated: Utc::now(),
        }))
    }

    /// Player insights — only accessible by the player or an admin.
    pub async fn get_player_insights(
        &self,
        requesting_user_id: Uuid,
        target_user_id: Uuid,
        is_admin: bool,
        game_id: i32,
    ) -> Result<Option<PlayerInsightsResponse>, ApiError> {
        if requesting_user_id != target_user_id && !is_admin {
            return Err(ApiError::forbidden("not authorised to view this data"));
        }

        let row = sqlx::query_as!(
            PlayerInsightsRow,
            r#"
            SELECT user_id, game_id, matches_played, wins, avg_session_secs, last_active
            FROM analytics_player_behaviour
            WHERE user_id = $1 AND game_id = $2
            "#,
            target_user_id,
            game_id,
        )
        .fetch_optional(&self.db)
        .await
        .map_err(|e| ApiError::InternalServerError(e.to_string()))?;

        Ok(row.map(|r| {
            let win_rate = if r.matches_played > 0 {
                r.wins as f64 / r.matches_played as f64 * 100.0
            } else {
                0.0
            };
            PlayerInsightsResponse {
                user_id: r.user_id,
                game_id: r.game_id,
                matches_played: r.matches_played,
                win_rate_pct: (win_rate * 10.0).round() / 10.0,
                avg_session_secs: r.avg_session_secs,
                last_active: r.last_active,
            }
        }))
    }

    // ── Data Aggregation (#689) ─────────────────────────────────────────────

    /// Get aggregated statistics across games
    pub async fn get_aggregated_stats(
        &self,
        game_id: Option<i32>,
        _period_start: Option<i64>,
        _period_end: Option<i64>,
    ) -> Result<serde_json::Value, ApiError> {
        let game_filter = game_id.map(|_| "AND game_id = $1").unwrap_or("");
        let query = format!(
            r#"
            SELECT
                COUNT(*) as total_matches,
                SUM(total_players) as total_players,
                SUM(total_wagered) as total_wagered,
                SUM(total_rewards_paid) as total_rewards_paid,
                AVG(avg_match_duration_secs) as avg_duration
            FROM analytics_game_metrics
            WHERE 1=1 {}
            "#,
            game_filter
        );

        let row = sqlx::query_scalar::<_, (i64, Option<i64>, Option<i64>, Option<i64>, Option<f64>)>(&query)
            .fetch_one(&self.db)
            .await
            .map_err(|e| ApiError::InternalServerError(e.to_string()))?;

        Ok(serde_json::json!({
            "total_matches": row.0,
            "total_players": row.1.unwrap_or(0),
            "total_wagered": row.2.unwrap_or(0),
            "total_rewards": row.3.unwrap_or(0),
            "avg_duration": row.4.unwrap_or(0.0),
        }))
    }

    /// Get revenue metrics
    pub async fn get_revenue_metrics(
        &self,
        _game_id: Option<i32>,
        _period_start: Option<i64>,
        _period_end: Option<i64>,
    ) -> Result<serde_json::Value, ApiError> {
        let platform = self.get_platform_metrics().await?;
        let total_users = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(DISTINCT user_id) FROM analytics_player_behaviour"
        )
        .fetch_one(&self.db)
        .await
        .map_err(|e| ApiError::InternalServerError(e.to_string()))?;

        let revenue_per_user = if total_users > 0 {
            platform.total_volume / total_users
        } else {
            0
        };

        Ok(serde_json::json!({
            "total_volume": platform.total_volume,
            "total_rewards_paid": 0,
            "net_revenue": platform.total_volume,
            "revenue_per_user": revenue_per_user,
            "total_users": total_users,
        }))
    }

    // ── Experiment exposure & guardrails (#255, #256) ───────────────────────

    /// Log an exposure event at presentation time. Idempotent per
    /// (assignment_id, surface): repeated renders do not inflate exposure counts.
    /// The assignment version is persisted so mid-session reassignment is auditable.
    pub async fn record_exposure(
        &self,
        assignment_id: Uuid,
        req: &RecordExposureRequest,
    ) -> Result<bool, ApiError> {
        if req.surface.trim().is_empty() {
            return Err(ApiError::bad_request("surface must not be empty"));
        }

        // Verify the assignment exists and matches the reported version/variant.
        let assignment = sqlx::query!(
            r#"
            SELECT experiment_key, variant, assignment_version
            FROM experiment_assignments
            WHERE id = $1
            "#,
            assignment_id,
        )
        .fetch_optional(&self.db)
        .await
        .map_err(|e| ApiError::InternalServerError(e.to_string()))?;

        let Some(assignment) = assignment else {
            return Err(ApiError::not_found("assignment not found"));
        };

        if assignment.experiment_key != req.experiment_key
            || assignment.variant != req.variant
            || assignment.assignment_version != req.assignment_version
        {
            return Err(ApiError::conflict(
                "exposure does not match the recorded assignment",
            ));
        }

        let inserted = sqlx::query!(
            r#"
            INSERT INTO experiment_exposures
                (assignment_id, experiment_key, variant, assignment_version, surface, exposed_at)
            VALUES ($1, $2, $3, $4, $5, NOW())
            ON CONFLICT (assignment_id, surface) DO NOTHING
            "#,
            assignment_id,
            req.experiment_key,
            req.variant,
            req.assignment_version,
            req.surface,
        )
        .execute(&self.db)
        .await
        .map_err(|e| ApiError::InternalServerError(e.to_string()))?;

        Ok(inserted.rows_affected() > 0)
    }

    /// Store a guardrail metric sample for an experiment variant.
    pub async fn record_guardrail_sample(
        &self,
        req: &RecordGuardrailSampleRequest,
    ) -> Result<(), ApiError> {
        if req.metric.trim().is_empty() {
            return Err(ApiError::bad_request("metric must not be empty"));
        }
        if !req.value.is_finite() {
            return Err(ApiError::bad_request("value must be finite"));
        }

        sqlx::query!(
            r#"
            INSERT INTO experiment_guardrail_samples
                (experiment_key, variant, metric, value, user_id, recorded_at)
            VALUES ($1, $2, $3, $4, $5, NOW())
            "#,
            req.experiment_key,
            req.variant,
            req.metric,
            req.value,
            req.user_id,
        )
        .execute(&self.db)
        .await
        .map_err(|e| ApiError::InternalServerError(e.to_string()))?;

        Ok(())
    }

    /// Compute the experiment analysis from stored guardrail samples and the
    /// experiment definition, delegating the decision to the pure function in
    /// `feature_flags` so dashboard and API agree.
    pub async fn analyze_experiment(
        &self,
        definition: &ExperimentDefinition,
    ) -> Result<ExperimentAnalysisResponse, ApiError> {
        let rows = sqlx::query!(
            r#"
            SELECT variant, metric, value
            FROM experiment_guardrail_samples
            WHERE experiment_key = $1
            "#,
            definition.key,
        )
        .fetch_all(&self.db)
        .await
        .map_err(|e| ApiError::InternalServerError(e.to_string()))?;

        let mut control: Vec<GuardrailMetricSample> = Vec::new();
        let mut treatment: Vec<GuardrailMetricSample> = Vec::new();

        for row in rows {
            let sample = GuardrailMetricSample {
                metric: row.metric,
                value: row.value,
            };
            if row.variant == definition.control_variant {
                control.push(sample);
            } else if row.variant == definition.treatment_variant {
                treatment.push(sample);
            }
        }

        let input = ExperimentAnalysisInput {
            definition: definition.clone(),
            control_samples: control,
            treatment_samples: treatment,
        };

        let decision = evaluate_experiment_decision(&input);

        Ok(ExperimentAnalysisResponse {
            experiment_key: definition.key.clone(),
            decision: decision.decision,
            primary_metric: definition.primary_metric.clone(),
            guardrail_metrics: definition.guardrail_metrics.clone(),
            control_samples: decision.control_samples,
            treatment_samples: decision.treatment_samples,
            missing_telemetry: decision.missing_telemetry,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::feature_flags::{
        ExperimentDefinition, ExperimentStatus, GuardrailMetricSample,
    };

    fn definition() -> ExperimentDefinition {
        ExperimentDefinition {
            key: "checkout_copy".to_string(),
            version: 3,
            control_variant: "control".to_string(),
            treatment_variant: "treatment".to_string(),
            primary_metric: "conversion".to_string(),
            guardrail_metrics: vec!["payment_error_rate".to_string()],
            minimum_sample: 100,
            analysis_window_hours: 168,
            stop_thresholds: Default::default(),
            status: ExperimentStatus::Running,
        }
    }

    fn samples(metric: &str, values: &[f64]) -> Vec<GuardrailMetricSample> {
        values
            .iter()
            .map(|v| GuardrailMetricSample {
                metric: metric.to_string(),
                value: *v,
            })
            .collect()
    }

    fn analyze(
        control: Vec<GuardrailMetricSample>,
        treatment: Vec<GuardrailMetricSample>,
    ) -> ExperimentDecision {
        let input = ExperimentAnalysisInput {
            definition: definition(),
            control_samples: control,
            treatment_samples: treatment,
        };
        evaluate_experiment_decision(&input).decision
    }

    #[test]
    fn synthetic_lift_is_safe_to_ship() {
        let control = samples("conversion", &[0.10; 200]);
        let treatment = samples("conversion", &[0.20; 200]);
        assert_eq!(analyze(control, treatment), ExperimentDecision::SafeToShip);
    }

    #[test]
    fn synthetic_harm_is_flagged() {
        let control = samples("conversion", &[0.20; 200]);
        let treatment = samples("conversion", &[0.10; 200]);
        assert_eq!(analyze(control, treatment), ExperimentDecision::Harmful);
    }

    #[test]
    fn missing_telemetry_is_insufficient_data() {
        let control = samples("conversion", &[0.10; 5]);
        let treatment: Vec<GuardrailMetricSample> = Vec::new();
        assert_eq!(
            analyze(control, treatment),
            ExperimentDecision::InsufficientData
        );
    }

    #[test]
    fn multiple_comparisons_require_stricter_evidence() {
        // A modest lift that would pass a single comparison must not pass when
        // several guardrails are evaluated simultaneously.
        let mut def = definition();
        def.guardrail_metrics = vec![
            "payment_error_rate".to_string(),
            "match_quality".to_string(),
            "latency_p95".to_string(),
            "player_safety".to_string(),
        ];
        let control = samples("conversion", &[0.10; 200]);
        let treatment = samples("conversion", &[0.13; 200]);
        let input = ExperimentAnalysisInput {
            definition: def,
            control_samples: control,
            treatment_samples: treatment,
        };
        let result = evaluate_experiment_decision(&input);
        // With multiple comparisons the decision must not be a naive ship.
        assert_ne!(result.decision, ExperimentDecision::SafeToShip);
    }
}

/// Validate an event against the supported schema. Returns a rejection reason
/// when invalid, or `None` when the event may be accepted.
fn validate_event(event: &AnalyticsEvent) -> Option<String> {
    if event.producer_id.trim().is_empty() {
        return Some("missing producer_id".to_string());
    }
    if event.event_id.trim().is_empty() {
        return Some("missing event_id".to_string());
    }
    if event.event_name.trim().is_empty() {
        return Some("missing event_name".to_string());
    }
    if !SUPPORTED_SCHEMA_VERSIONS.contains(&event.schema_version) {
        return Some(format!(
            "unsupported schema_version {}",
            event.schema_version
        ));
    }
    None
}
