use crate::{
    api_error::ApiError,
    auth::middleware::ClaimsExt,
    service::{
        account_recovery::AccountRecoveryService,
        notification_service::NotificationService,
    },
};
use actix_web::{web, HttpRequest, HttpResponse};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct IssueCodesRequest {
    pub current_password: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct StartRecoveryRequest {
    pub recovery_code: String,
}

#[derive(Debug, Deserialize)]
pub struct CompleteRecoveryRequest {
    pub case_id: Uuid,
    pub recovery_code: String,
    pub new_password: String,
}

#[derive(Debug, Deserialize)]
pub struct AppealRequest {
    pub case_id: Uuid,
    pub recovery_code: String,
}

#[derive(Debug, Deserialize)]
pub struct ResolveAppealRequest {
    pub case_id: Uuid,
    pub approve: bool,
}

#[derive(Debug, Serialize)]
struct StartRecoveryResponse {
    message: &'static str,
    case_id: Option<Uuid>,
    eligible_at: Option<chrono::DateTime<chrono::Utc>>,
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
    notification_status: &'static str,
}

pub async fn issue_codes(
    recovery: web::Data<AccountRecoveryService>,
    notifications: web::Data<NotificationService>,
    request: HttpRequest,
    body: web::Json<IssueCodesRequest>,
) -> Result<HttpResponse, ApiError> {
    let user_id = request
        .user_id()
        .ok_or_else(|| ApiError::unauthorized("User not authenticated"))?;
    let codes = recovery
        .issue_codes(user_id, body.current_password.as_deref())
        .await?;
    let delivery = notifications
        .send_security_notice(
            user_id,
            "Account recovery codes changed",
            "A new set of account recovery codes was issued. Existing unused recovery codes were invalidated. If you did not request this, contact support immediately.",
        )
        .await;
    match delivery {
        Ok(()) => Ok(HttpResponse::Ok().json(serde_json::json!({
            "codes": codes.codes,
            "notification_status": "sent"
        }))),
        Err(_) => Ok(HttpResponse::ServiceUnavailable().json(serde_json::json!({
            "codes": codes.codes,
            "notification_status": "failed",
            "error": "The codes were created, but the security notice could not be delivered."
        }))),
    }
}

pub async fn start_recovery(
    recovery: web::Data<AccountRecoveryService>,
    notifications: web::Data<NotificationService>,
    body: web::Json<StartRecoveryRequest>,
) -> Result<HttpResponse, ApiError> {
    let Some(case) = recovery.start_case(&body.recovery_code).await? else {
        return Ok(HttpResponse::Accepted().json(StartRecoveryResponse {
            message: "If the recovery proof is valid, instructions have been sent.",
            case_id: None,
            eligible_at: None,
            expires_at: None,
            notification_status: "unknown",
        }));
    };

    match notifications
        .send_security_notice(
            case.user_id,
            "Account recovery requested",
            &format!(
                "An account recovery was requested. The request cannot complete before {} UTC. If you did not request this, use a recovery code to appeal the case or contact security@arenax.gg with case reference {}.",
                case.eligible_at.to_rfc3339(),
                case.id
            ),
        )
        .await
    {
        Ok(()) => recovery.record_notification_result(case.id, true).await?,
        Err(_) => {
            recovery.record_notification_result(case.id, false).await?;
            return Ok(HttpResponse::ServiceUnavailable().json(StartRecoveryResponse {
                message: "Recovery case created, but the security notification could not be delivered.",
                case_id: Some(case.id),
                eligible_at: Some(case.eligible_at),
                expires_at: Some(case.expires_at),
                notification_status: "failed",
            }));
        }
    }

    Ok(HttpResponse::Accepted().json(StartRecoveryResponse {
        message: "Recovery request accepted; the cooling period must elapse before completion.",
        case_id: Some(case.id),
        eligible_at: Some(case.eligible_at),
        expires_at: Some(case.expires_at),
        notification_status: "sent",
    }))
}

pub async fn complete_recovery(
    recovery: web::Data<AccountRecoveryService>,
    notifications: web::Data<NotificationService>,
    body: web::Json<CompleteRecoveryRequest>,
) -> Result<HttpResponse, ApiError> {
    let user_id = recovery
        .complete_case(body.case_id, &body.recovery_code, &body.new_password)
        .await?;
    match notifications
        .send_security_notice(
            user_id,
            "Account recovery completed",
            "Your account password was changed through account recovery and all active sessions were revoked. If this was not you, contact security@arenax.gg immediately.",
        )
        .await
    {
        Ok(()) => Ok(HttpResponse::Ok().json(serde_json::json!({
            "recovery_status": "completed",
            "notification_status": "sent"
        }))),
        Err(_) => Ok(HttpResponse::ServiceUnavailable().json(serde_json::json!({
            "recovery_status": "completed",
            "notification_status": "failed",
            "error": "Password changed and sessions revoked, but the security notice could not be delivered."
        }))),
    }
}

pub async fn appeal_recovery(
    recovery: web::Data<AccountRecoveryService>,
    body: web::Json<AppealRequest>,
) -> Result<HttpResponse, ApiError> {
    recovery
        .appeal_case(body.case_id, &body.recovery_code)
        .await?;
    Ok(HttpResponse::Accepted().json(serde_json::json!({
        "message": "Appeal recorded. Contact security@arenax.gg with the case reference for human review."
    })))
}

pub async fn resolve_appeal(
    recovery: web::Data<AccountRecoveryService>,
    notifications: web::Data<NotificationService>,
    request: HttpRequest,
    body: web::Json<ResolveAppealRequest>,
) -> Result<HttpResponse, ApiError> {
    let claims = request
        .claims()
        .ok_or_else(|| ApiError::unauthorized("User not authenticated"))?;
    if !claims
        .roles
        .iter()
        .any(|role| role == "admin" || role == "super_admin")
    {
        return Err(ApiError::forbidden("Admin access required"));
    }
    let reviewer =
        Uuid::parse_str(&claims.sub).map_err(|_| ApiError::unauthorized("Invalid user ID"))?;
    let user_id = recovery
        .resolve_appeal(body.case_id, reviewer, body.approve)
        .await?;
    let (title, message) = if body.approve {
        (
            "Account recovery appeal approved",
            "A support reviewer approved your recovery appeal. Your recovery case remains subject to its cooling period.",
        )
    } else {
        (
            "Account recovery appeal denied",
            "A support reviewer denied your recovery appeal. Contact security@arenax.gg if you need further help.",
        )
    };
    notifications
        .send_security_notice(user_id, title, message)
        .await?;
    Ok(HttpResponse::NoContent().finish())
}

pub fn configure_routes(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/auth/recovery")
            .route("/codes", web::post().to(issue_codes))
            .route("/start", web::post().to(start_recovery))
            .route("/complete", web::post().to(complete_recovery))
            .route("/appeal", web::post().to(appeal_recovery))
            .route("/appeal/review", web::post().to(resolve_appeal)),
    );
}
