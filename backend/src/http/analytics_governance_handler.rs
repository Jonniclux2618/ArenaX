use crate::{
    api_error::ApiError,
    auth::middleware::ClaimsExt,
    service::analytics_governance::{
        AnalyticsGovernanceService, DatasetContractInput, QualityEventInput,
        QualityThresholdInput, RetentionExceptionInput,
    },
};
use actix_web::{web, HttpRequest, HttpResponse};
use serde::Deserialize;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct AccessGrantRequest {
    pub dataset_name: String,
    pub user_id: Uuid,
    pub review_days: i32,
}

#[derive(Debug, Deserialize)]
pub struct AccessRevokeRequest {
    pub dataset_name: String,
    pub user_id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct RetentionPurgeRequest {
    pub dataset_name: String,
    pub dry_run: bool,
    pub batch_limit: i64,
}

#[derive(Debug, Deserialize)]
pub struct ReviewExceptionRequest {
    pub approve: bool,
}

#[derive(Debug, Deserialize)]
pub struct QualitySummaryQuery {
    pub dataset_name: String,
    pub event_type: String,
}

fn admin_id(request: &HttpRequest) -> Result<Uuid, ApiError> {
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
    Uuid::parse_str(&claims.sub).map_err(|_| ApiError::unauthorized("Invalid user ID"))
}

fn actor_id(request: &HttpRequest) -> Result<Uuid, ApiError> {
    request
        .user_id()
        .ok_or_else(|| ApiError::unauthorized("User not authenticated"))
}

pub async fn put_contract(
    db: web::Data<sqlx::PgPool>,
    request: HttpRequest,
    body: web::Json<DatasetContractInput>,
) -> Result<HttpResponse, ApiError> {
    let service = AnalyticsGovernanceService::new(db.get_ref().clone());
    service.put_contract(&body, admin_id(&request)?).await?;
    Ok(HttpResponse::NoContent().finish())
}

pub async fn list_contracts(
    db: web::Data<sqlx::PgPool>,
    request: HttpRequest,
) -> Result<HttpResponse, ApiError> {
    admin_id(&request)?;
    let service = AnalyticsGovernanceService::new(db.get_ref().clone());
    Ok(HttpResponse::Ok().json(service.list_contracts().await?))
}

pub async fn grant_access(
    db: web::Data<sqlx::PgPool>,
    request: HttpRequest,
    body: web::Json<AccessGrantRequest>,
) -> Result<HttpResponse, ApiError> {
    let id = admin_id(&request)?;
    let service = AnalyticsGovernanceService::new(db.get_ref().clone());
    let grant_id = service
        .grant_access(&body.dataset_name, body.user_id, id, body.review_days)
        .await?;
    Ok(HttpResponse::Created().json(serde_json::json!({ "grant_id": grant_id })))
}

pub async fn revoke_access(
    db: web::Data<sqlx::PgPool>,
    request: HttpRequest,
    body: web::Json<AccessRevokeRequest>,
) -> Result<HttpResponse, ApiError> {
    let id = admin_id(&request)?;
    let service = AnalyticsGovernanceService::new(db.get_ref().clone());
    service
        .revoke_access(&body.dataset_name, body.user_id, id)
        .await?;
    Ok(HttpResponse::NoContent().finish())
}

pub async fn list_access(
    db: web::Data<sqlx::PgPool>,
    request: HttpRequest,
) -> Result<HttpResponse, ApiError> {
    admin_id(&request)?;
    let service = AnalyticsGovernanceService::new(db.get_ref().clone());
    Ok(HttpResponse::Ok().json(service.list_access_grants().await?))
}

pub async fn put_threshold(
    db: web::Data<sqlx::PgPool>,
    request: HttpRequest,
    body: web::Json<QualityThresholdInput>,
) -> Result<HttpResponse, ApiError> {
    admin_id(&request)?;
    let service = AnalyticsGovernanceService::new(db.get_ref().clone());
    service.put_threshold(&body).await?;
    Ok(HttpResponse::NoContent().finish())
}

pub async fn publish_event(
    db: web::Data<sqlx::PgPool>,
    request: HttpRequest,
    body: web::Json<QualityEventInput>,
) -> Result<HttpResponse, ApiError> {
    let claims = request
        .claims()
        .ok_or_else(|| ApiError::unauthorized("User not authenticated"))?;
    let user_id = actor_id(&request)?;
    let is_admin = claims
        .roles
        .iter()
        .any(|role| role == "admin" || role == "super_admin");
    let service = AnalyticsGovernanceService::new(db.get_ref().clone());
    let result = service.publish_event(&body, user_id, is_admin).await?;
    let status = match result.status.as_str() {
        "healthy" => actix_web::http::StatusCode::ACCEPTED,
        "degraded" => actix_web::http::StatusCode::ACCEPTED,
        _ => actix_web::http::StatusCode::UNPROCESSABLE_ENTITY,
    };
    Ok(HttpResponse::build(status).json(result))
}

pub async fn quality_summary(
    db: web::Data<sqlx::PgPool>,
    request: HttpRequest,
    query: web::Query<QualitySummaryQuery>,
) -> Result<HttpResponse, ApiError> {
    let claims = request
        .claims()
        .ok_or_else(|| ApiError::unauthorized("User not authenticated"))?;
    let user_id = actor_id(&request)?;
    let is_admin = claims
        .roles
        .iter()
        .any(|role| role == "admin" || role == "super_admin");
    let service = AnalyticsGovernanceService::new(db.get_ref().clone());
    Ok(HttpResponse::Ok().json(
        service
            .quality_summary(&query.dataset_name, &query.event_type, user_id, is_admin)
            .await?,
    ))
}

pub async fn run_purge(
    db: web::Data<sqlx::PgPool>,
    request: HttpRequest,
    body: web::Json<RetentionPurgeRequest>,
) -> Result<HttpResponse, ApiError> {
    admin_id(&request)?;
    let service = AnalyticsGovernanceService::new(db.get_ref().clone());
    Ok(HttpResponse::Ok().json(
        service
            .run_retention_purge(&body.dataset_name, body.dry_run, body.batch_limit)
            .await?,
    ))
}

pub async fn create_retention_exception(
    db: web::Data<sqlx::PgPool>,
    request: HttpRequest,
    body: web::Json<RetentionExceptionInput>,
) -> Result<HttpResponse, ApiError> {
    let id = admin_id(&request)?;
    let service = AnalyticsGovernanceService::new(db.get_ref().clone());
    let exception_id = service.create_retention_exception(&body, id).await?;
    Ok(HttpResponse::Created().json(serde_json::json!({ "exception_id": exception_id })))
}

pub async fn review_retention_exception(
    db: web::Data<sqlx::PgPool>,
    request: HttpRequest,
    exception_id: web::Path<Uuid>,
    body: web::Json<ReviewExceptionRequest>,
) -> Result<HttpResponse, ApiError> {
    let id = admin_id(&request)?;
    let service = AnalyticsGovernanceService::new(db.get_ref().clone());
    service
        .review_retention_exception(exception_id.into_inner(), id, body.approve)
        .await?;
    Ok(HttpResponse::NoContent().finish())
}

pub async fn list_retention_runs(
    db: web::Data<sqlx::PgPool>,
    request: HttpRequest,
) -> Result<HttpResponse, ApiError> {
    admin_id(&request)?;
    let service = AnalyticsGovernanceService::new(db.get_ref().clone());
    Ok(HttpResponse::Ok().json(service.list_retention_runs().await?))
}

pub fn configure_routes(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/analytics/governance")
            .route("/datasets", web::get().to(list_contracts))
            .route("/datasets", web::put().to(put_contract))
            .route("/access", web::get().to(list_access))
            .route("/access/grant", web::post().to(grant_access))
            .route("/access/revoke", web::post().to(revoke_access))
            .route("/thresholds", web::put().to(put_threshold))
            .route("/events", web::post().to(publish_event))
            .route("/events/summary", web::get().to(quality_summary))
            .route("/retention/purge", web::post().to(run_purge))
            .route("/retention/runs", web::get().to(list_retention_runs))
            .route(
                "/retention/exceptions",
                web::post().to(create_retention_exception),
            )
            .route(
                "/retention/exceptions/{exception_id}/review",
                web::post().to(review_retention_exception),
            ),
    );
}
