use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::error::ApiError;

/// A single player-safe contributing factor for a rating delta.
/// Only persisted, non-confidential fields are surfaced here.
#[derive(Debug, Serialize, Deserialize)]
pub struct RatingFactor {
    pub label: String,
    pub detail: String,
}

/// Explanation of a rating change, derived entirely from persisted inputs
/// (finalized match, rating version, stored factors) so it stays stable
/// even after future model changes.
#[derive(Debug, Serialize, Deserialize)]
pub struct RatingExplanation {
    pub match_id: Uuid,
    pub rating_version: String,
    pub delta: f64,
    pub factors: Vec<RatingFactor>,
}

#[derive(Debug, sqlx::FromRow)]
struct RatingSnapshotRow {
    match_id: Uuid,
    rating_version: String,
    delta: f64,
    factors: serde_json::Value,
}

/// GET /api/ratings/:player_id/explanations/:match_id
///
/// Returns a reproducible explanation for a player's rating delta in a
/// finalized match. Reads only persisted snapshot data; never recomputes
/// from the current model and never exposes moderation/anti-abuse fields.
pub async fn get_rating_explanation(
    State(pool): State<PgPool>,
    auth: AuthUser,
    Path((player_id, match_id)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    // Authorization: a player may view their own explanation; operators may
    // view any player's explanation.
    if auth.user_id != player_id && !auth.is_operator {
        return Err(ApiError::Forbidden);
    }

    let row = sqlx::query_as::<_, RatingSnapshotRow>(
        r#"
        SELECT match_id, rating_version, delta, factors
        FROM rating_snapshots
        WHERE player_id = $1 AND match_id = $2
        "#,
    )
    .bind(player_id)
    .bind(match_id)
    .fetch_optional(&pool)
    .await
    .map_err(ApiError::from)?;

    let row = match row {
        Some(row) => row,
        None => return Err(ApiError::NotFound),
    };

    // Factors are stored as a JSON array of {label, detail} objects at
    // snapshot time. Deserialize defensively so a malformed row does not
    // leak internal data or crash the endpoint.
    let factors: Vec<RatingFactor> = serde_json::from_value(row.factors)
        .map_err(|_| ApiError::Internal)?;

    let explanation = RatingExplanation {
        match_id: row.match_id,
        rating_version: row.rating_version,
        delta: row.delta,
        factors,
    };

    Ok((StatusCode::OK, Json(explanation)))
}
