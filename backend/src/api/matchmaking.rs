use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::services::match_service::{MatchService, QueueExitOutcome, QueueExitReason};

#[derive(Debug, Deserialize)]
pub struct JoinQueueRequest {
    pub mode: String,
    /// Client-supplied idempotency key so retries cannot duplicate queue entries.
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct QueueExitResponse {
    pub player_id: Uuid,
    pub outcome: String,
    pub reason: String,
    pub queue_position: Option<u32>,
    pub reservation_released: bool,
    pub message: String,
}

impl QueueExitResponse {
    fn from_outcome(player_id: Uuid, outcome: QueueExitOutcome) -> Self {
        let (outcome_label, message) = match outcome.reason {
            QueueExitReason::CancelledQueued => (
                "cancelled",
                "You left the queue. Your queue position was released.",
            ),
            QueueExitReason::CancelledMatched => (
                "cancelled",
                "You left after being matched. The match was voided and other players were requeued.",
            ),
            QueueExitReason::CancelledReserved => (
                "cancelled",
                "You left during reservation. The reservation was released and eligible players were restored.",
            ),
            QueueExitReason::TimedOutQueued => (
                "timed_out",
                "Your queue wait timed out. You may rejoin without losing fairness standing.",
            ),
            QueueExitReason::TimedOutMatched => (
                "timed_out",
                "You did not accept the match in time. The match was voided and other players were requeued.",
            ),
            QueueExitReason::TimedOutReserved => (
                "timed_out",
                "Your reservation expired. The reservation was released and eligible players were restored.",
            ),
            QueueExitReason::NotInQueue => (
                "noop",
                "You were not in the queue; nothing changed.",
            ),
        };

        Self {
            player_id,
            outcome: outcome_label.to_string(),
            reason: outcome.reason.as_str().to_string(),
            queue_position: outcome.queue_position,
            reservation_released: outcome.reservation_released,
            message: message.to_string(),
        }
    }
}

pub async fn join_queue(
    State(service): State<Arc<MatchService>>,
    AuthUser { user_id }: AuthUser,
    Json(req): Json<JoinQueueRequest>,
) -> impl IntoResponse {
    match service
        .join_queue(user_id, &req.mode, req.idempotency_key.as_deref())
        .await
    {
        Ok(entry) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "player_id": entry.player_id,
                "queue_position": entry.queue_position,
                "state": entry.state.as_str(),
            })),
        )
            .into_response(),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": err.to_string() })),
        )
            .into_response(),
    }
}

/// Cancel the caller's current queue participation (queued, matched, or reserved).
/// Idempotent: repeated cancels return a `noop` outcome instead of erroring.
pub async fn cancel_queue(
    State(service): State<Arc<MatchService>>,
    AuthUser { user_id }: AuthUser,
) -> impl IntoResponse {
    match service.cancel_queue(user_id).await {
        Ok(outcome) => (
            StatusCode::OK,
            Json(QueueExitResponse::from_outcome(user_id, outcome)),
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": err.to_string() })),
        )
            .into_response(),
    }
}

/// Explicit timeout endpoint used by clients that detect their own deadline,
/// and by workers replaying a timeout. Deterministic and idempotent.
pub async fn timeout_queue(
    State(service): State<Arc<MatchService>>,
    AuthUser { user_id }: AuthUser,
) -> impl IntoResponse {
    match service.timeout_queue(user_id).await {
        Ok(outcome) => (
            StatusCode::OK,
            Json(QueueExitResponse::from_outcome(user_id, outcome)),
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": err.to_string() })),
        )
            .into_response(),
    }
}

/// Report the caller's current queue-exit-relevant state so clients can render
/// deterministic UI for queued, matched, and reserved players.
pub async fn queue_status(
    State(service): State<Arc<MatchService>>,
    AuthUser { user_id }: AuthUser,
) -> impl IntoResponse {
    match service.queue_status(user_id).await {
        Ok(status) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "player_id": user_id,
                "state": status.state.as_str(),
                "queue_position": status.queue_position,
                "reservation_expires_at": status.reservation_expires_at,
            })),
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": err.to_string() })),
        )
            .into_response(),
    }
}

/// Operator-facing reservation expiry sweep. Runs the same cleanup path used by
/// the scheduler so replaying it cannot skip fairness rules or double-restore.
pub async fn expire_reservations(
    State(service): State<Arc<MatchService>>,
    AuthUser { user_id }: AuthUser,
) -> impl IntoResponse {
    if !service.is_operator(user_id).await {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "operator role required" })),
        )
            .into_response();
    }

    match service.expire_reservations().await {
        Ok(expired) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "expired": expired
                    .into_iter()
                    .map(|o| QueueExitResponse::from_outcome(o.player_id, o))
                    .collect::<Vec<_>>(),
            })),
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": err.to_string() })),
        )
            .into_response(),
    }
}

pub async fn get_match(
    State(service): State<Arc<MatchService>>,
    Path(match_id): Path<Uuid>,
) -> impl IntoResponse {
    match service.get_match(match_id).await {
        Ok(Some(m)) => (StatusCode::OK, Json(m)).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "match not found" })),
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": err.to_string() })),
        )
            .into_response(),
    }
}
