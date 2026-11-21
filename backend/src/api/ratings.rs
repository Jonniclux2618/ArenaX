use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

/// Versioned, bounded calibration parameters for uncertainty-aware placement.
///
/// These parameters govern how provisional ratings and rating uncertainty are
/// derived for new and returning players. They are versioned so historical
/// outcomes can be evaluated against a specific calibration without
/// retroactively mutating finalized results.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CalibrationParams {
    /// Version identifier persisted alongside derived ratings.
    pub version: u32,
    /// Conservative starting rating for new accounts.
    pub provisional_rating: f64,
    /// Elevated starting uncertainty for new accounts.
    pub provisional_uncertainty: f64,
    /// Minimum uncertainty floor (never decreases below this).
    pub min_uncertainty: f64,
    /// Maximum uncertainty ceiling.
    pub max_uncertainty: f64,
    /// Multiplicative decay applied per eligible finalized match.
    pub uncertainty_decay: f64,
}

impl CalibrationParams {
    pub const CURRENT_VERSION: u32 = 1;

    /// Bounds for each parameter, used to clamp operator-supplied values.
    const RATING_BOUNDS: (f64, f64) = (0.0, 4000.0);
    const UNCERTAINTY_BOUNDS: (f64, f64) = (1.0, 1000.0);
    const DECAY_BOUNDS: (f64, f64) = (0.5, 1.0);

    /// Construct a calibration, clamping every field into safe ranges.
    pub fn bounded(
        version: u32,
        provisional_rating: f64,
        provisional_uncertainty: f64,
        min_uncertainty: f64,
        max_uncertainty: f64,
        uncertainty_decay: f64,
    ) -> Self {
        let (rmin, rmax) = Self::RATING_BOUNDS;
        let (umin, umax) = Self::UNCERTAINTY_BOUNDS;
        let (dmin, dmax) = Self::DECAY_BOUNDS;

        let max_uncertainty = max_uncertainty.clamp(umin, umax);
        let min_uncertainty = min_uncertainty.clamp(umin, max_uncertainty);

        Self {
            version,
            provisional_rating: provisional_rating.clamp(rmin, rmax),
            provisional_uncertainty: provisional_uncertainty.clamp(min_uncertainty, max_uncertainty),
            min_uncertainty,
            max_uncertainty,
            uncertainty_decay: uncertainty_decay.clamp(dmin, dmax),
        }
    }

    /// Default conservative placement for new accounts.
    pub fn default_placement() -> Self {
        Self::bounded(Self::CURRENT_VERSION, 1000.0, 350.0, 25.0, 400.0, 0.85)
    }

    /// Apply one eligible finalized match to the current uncertainty.
    pub fn decay_uncertainty(&self, current: f64) -> f64 {
        (current * self.uncertainty_decay).clamp(self.min_uncertainty, self.max_uncertainty)
    }
}

#[derive(Debug, Serialize)]
pub struct PlacementResponse {
    pub player_id: Uuid,
    pub rating: f64,
    pub uncertainty: f64,
    pub provisional: bool,
    pub calibration_version: u32,
}

#[derive(Debug, Deserialize)]
pub struct UpdateCalibrationRequest {
    pub provisional_rating: f64,
    pub provisional_uncertainty: f64,
    pub min_uncertainty: f64,
    pub max_uncertainty: f64,
    pub uncertainty_decay: f64,
}

#[derive(Debug, Serialize)]
pub struct CalibrationResponse {
    pub version: u32,
    pub provisional_rating: f64,
    pub provisional_uncertainty: f64,
    pub min_uncertainty: f64,
    pub max_uncertainty: f64,
    pub uncertainty_decay: f64,
}

impl From<CalibrationParams> for CalibrationResponse {
    fn from(p: CalibrationParams) -> Self {
        Self {
            version: p.version,
            provisional_rating: p.provisional_rating,
            provisional_uncertainty: p.provisional_uncertainty,
            min_uncertainty: p.min_uncertainty,
            max_uncertainty: p.max_uncertainty,
            uncertainty_decay: p.uncertainty_decay,
        }
    }
}

#[derive(Debug)]
pub enum RatingError {
    NotFound,
    Db(sqlx::Error),
}

impl IntoResponse for RatingError {
    fn into_response(self) -> axum::response::Response {
        match self {
            RatingError::NotFound => {
                (StatusCode::NOT_FOUND, "player not found").into_response()
            }
            RatingError::Db(err) => {
                tracing::error!(error = %err, "rating service database error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
            }
        }
    }
}

impl From<sqlx::Error> for RatingError {
    fn from(err: sqlx::Error) -> Self {
        RatingError::Db(err)
    }
}

/// Canonical placement boundary: new accounts receive a conservative
/// provisional rating with elevated uncertainty derived from the active
/// calibration parameters.
pub async fn place_new_player(
    pool: &PgPool,
    player_id: Uuid,
) -> Result<PlacementResponse, RatingError> {
    let params = CalibrationParams::default_placement();

    sqlx::query(
        r#"
        INSERT INTO player_ratings (player_id, rating, uncertainty, provisional, calibration_version)
        VALUES ($1, $2, $3, TRUE, $4)
        ON CONFLICT (player_id) DO NOTHING
        "#,
    )
    .bind(player_id)
    .bind(params.provisional_rating)
    .bind(params.provisional_uncertainty)
    .bind(params.version as i32)
    .execute(pool)
    .await?;

    Ok(PlacementResponse {
        player_id,
        rating: params.provisional_rating,
        uncertainty: params.provisional_uncertainty,
        provisional: true,
        calibration_version: params.version,
    })
}

/// Apply uncertainty decay for a single eligible finalized match.
///
/// Only finalized, non-voided matches are eligible; callers must pass the
/// eligibility flag resolved from the match record. Ineligible matches leave
/// the rating untouched, preserving finalized results.
pub async fn apply_finalized_match(
    pool: &PgPool,
    player_id: Uuid,
    eligible: bool,
) -> Result<PlacementResponse, RatingError> {
    let row: Option<(f64, f64, bool, i32)> = sqlx::query_as(
        r#"
        SELECT rating, uncertainty, provisional, calibration_version
        FROM player_ratings
        WHERE player_id = $1
        "#,
    )
    .bind(player_id)
    .fetch_optional(pool)
    .await?;

    let (rating, uncertainty, provisional, version) = row.ok_or(RatingError::NotFound)?;

    if !eligible {
        return Ok(PlacementResponse {
            player_id,
            rating,
            uncertainty,
            provisional,
            calibration_version: version as u32,
        });
    }

    let params = CalibrationParams::default_placement();
    let new_uncertainty = params.decay_uncertainty(uncertainty);
    let still_provisional = new_uncertainty > params.min_uncertainty;

    sqlx::query(
        r#"
        UPDATE player_ratings
        SET uncertainty = $2, provisional = $3
        WHERE player_id = $1
        "#,
    )
    .bind(player_id)
    .bind(new_uncertainty)
    .bind(still_provisional)
    .execute(pool)
    .await?;

    Ok(PlacementResponse {
        player_id,
        rating,
        uncertainty: new_uncertainty,
        provisional: still_provisional,
        calibration_version: version as u32,
    })
}

pub async fn get_placement(
    State(pool): State<PgPool>,
    Path(player_id): Path<Uuid>,
) -> Result<Json<PlacementResponse>, RatingError> {
    let row: Option<(f64, f64, bool, i32)> = sqlx::query_as(
        r#"
        SELECT rating, uncertainty, provisional, calibration_version
        FROM player_ratings
        WHERE player_id = $1
        "#,
    )
    .bind(player_id)
    .fetch_optional(&pool)
    .await?;

    let (rating, uncertainty, provisional, version) = row.ok_or(RatingError::NotFound)?;

    Ok(Json(PlacementResponse {
        player_id,
        rating,
        uncertainty,
        provisional,
        calibration_version: version as u32,
    }))
}

/// Operator endpoint: preview bounded calibration parameters without mutating
/// finalized results. Values are clamped to safe ranges before being returned.
pub async fn preview_calibration(
    Json(req): Json<UpdateCalibrationRequest>,
) -> Json<CalibrationResponse> {
    let params = CalibrationParams::bounded(
        CalibrationParams::CURRENT_VERSION,
        req.provisional_rating,
        req.provisional_uncertainty,
        req.min_uncertainty,
        req.max_uncertainty,
        req.uncertainty_decay,
    );
    Json(params.into())
}
