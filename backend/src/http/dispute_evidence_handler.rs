//! Dispute evidence upload with tamper-evident hash storage — Issues #1081, #1076.
//!
//! - `POST /api/matches/{id}/dispute/evidence` — upload a screenshot as the
//!   raw request body (`Content-Type: image/png|image/jpeg|image/webp`).
//!   Creates the dispute if one does not already exist for this match/player,
//!   hashes the bytes with SHA-256, stores them in S3, and persists the hash
//!   + S3 key.
//! - `GET  /api/matches/{id}/dispute` — the most recent dispute for the
//!   match, including `evidence_hash` so the player (or an admin) can
//!   re-hash the S3 object and confirm it was never swapped.

use crate::api_error::ApiError;
use crate::auth::middleware::ClaimsExt;
use crate::db::DbPool;
use crate::models::match_models::{Match, MatchDispute};
use crate::service::evidence_storage::{
    evidence_object_key, hash_evidence, validate_content_type, validate_evidence_size,
    validate_magic_bytes, EvidenceStore,
};
use crate::service::evidence_download::EvidenceDownloadCapability;
use crate::service::match_service::MatchService;
use actix_web::{web, HttpRequest, HttpResponse};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;
use chrono::{DateTime, Utc};
use once_cell::sync::OnceCell;

static EVIDENCE_DOWNLOAD_CAPABILITY: OnceCell<EvidenceDownloadCapability> = OnceCell::new();

#[derive(Debug, Deserialize)]
pub struct EvidenceDownloadLinkRequest {}

#[derive(Debug, Serialize)]
pub struct EvidenceDownloadLinkResponse {
    pub capability: String,
    pub expires_in_seconds: i64,
}

#[derive(Debug, Deserialize)]
pub struct RedeemEvidenceDownloadRequest {
    pub capability: String,
}

#[derive(Debug, Serialize)]
pub struct DisputeEvidenceResponse {
    #[serde(flatten)]
    pub dispute: MatchDispute,
}

#[derive(Debug, Deserialize)]
pub struct EvidenceLegalHoldRequest {
    pub enabled: bool,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct EvidenceCustodyRecord {
    pub id: Uuid,
    pub dispute_id: Uuid,
    pub version: i32,
    pub sha256: String,
    pub object_key: String,
    pub uploaded_by: Uuid,
    pub uploaded_at: DateTime<Utc>,
    pub transformation_parent_id: Option<Uuid>,
    pub retention_class: String,
}

/// POST /api/matches/{id}/dispute/evidence
pub async fn upload_dispute_evidence(
    req: HttpRequest,
    db: web::Data<DbPool>,
    store: web::Data<Arc<dyn EvidenceStore>>,
    path: web::Path<Uuid>,
    body: web::Bytes,
) -> Result<HttpResponse, ApiError> {
    let match_id = path.into_inner();
    let user_id = req
        .user_id()
        .ok_or_else(|| ApiError::unauthorized("Authentication required"))?;

    authorize_match_player(db.get_ref(), match_id, user_id).await?;

    let content_type = req
        .headers()
        .get("Content-Type")
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default()
        .to_string();

    validate_content_type(&content_type)?;
    validate_evidence_size(body.len())?;
    validate_magic_bytes(&content_type, &body)?;

    let hash_hex = hash_evidence(&body);
    let s3_key = evidence_object_key(user_id, match_id, &content_type, &hash_hex);
    if let Err(error) = store.put(&s3_key, &content_type, &body).await {
        if let Err(cleanup_error) = store.delete(&s3_key).await {
            tracing::error!(
                object_key = %s3_key,
                error = %cleanup_error,
                "Could not clean up a potentially partial evidence upload"
            );
        }
        return Err(error);
    }

    let svc = MatchService::new(db.get_ref().clone());
    let dispute = match svc
        .attach_dispute_evidence(match_id, user_id, &hash_hex, &s3_key, user_id)
        .await
    {
        Ok(dispute) => dispute,
        Err(error) => {
            if let Err(cleanup_error) = store.delete(&s3_key).await {
                tracing::error!(
                    object_key = %s3_key,
                    error = %cleanup_error,
                    "Could not clean up evidence after its custody record failed"
                );
            }
            return Err(error);
        }
    };

    Ok(HttpResponse::Ok().json(DisputeEvidenceResponse { dispute }))
}

/// GET /api/matches/{id}/dispute
pub async fn get_match_dispute(
    db: web::Data<DbPool>,
    req: HttpRequest,
    path: web::Path<Uuid>,
) -> Result<HttpResponse, ApiError> {
    let match_id = path.into_inner();
    let user_id = req
        .user_id()
        .ok_or_else(|| ApiError::unauthorized("Authentication required"))?;
    let staff_access = has_staff_access(&req);
    authorize_evidence_access(db.get_ref(), match_id, user_id, staff_access).await?;
    let svc = MatchService::new(db.get_ref().clone());
    let disputes = svc.get_match_disputes(match_id).await?;
    let latest = disputes
        .into_iter()
        .find(|dispute| staff_access || dispute.disputing_player_id == user_id)
        .next()
        .ok_or_else(|| ApiError::not_found("No dispute found for this match"))?;

    sqlx::query(
        r#"
        INSERT INTO audit_logs (user_id, action, resource_type, resource_id, details)
        VALUES ($1, 'DISPUTE_EVIDENCE_METADATA_ACCESSED', 'match_dispute', $2, $3)
        "#,
    )
    .bind(user_id)
    .bind(latest.id)
    .bind(serde_json::json!({ "match_id": match_id }).to_string())
    .execute(db.get_ref())
    .await
    .map_err(ApiError::database_error)?;

    Ok(HttpResponse::Ok().json(DisputeEvidenceResponse { dispute: latest }))
}

/// GET /api/matches/{id}/dispute/evidence
pub async fn get_dispute_evidence(
    req: HttpRequest,
    db: web::Data<DbPool>,
    store: web::Data<Arc<dyn EvidenceStore>>,
    path: web::Path<Uuid>,
) -> Result<HttpResponse, ApiError> {
    let match_id = path.into_inner();
    let user_id = req
        .user_id()
        .ok_or_else(|| ApiError::unauthorized("Authentication required"))?;
    let staff_access = has_staff_access(&req);
    authorize_evidence_access(db.get_ref(), match_id, user_id, staff_access).await?;

    let dispute = sqlx::query_as!(
        MatchDispute,
        r#"
        SELECT * FROM match_disputes
                WHERE match_id = $1 AND ($2 OR disputing_player_id = $3)
          AND evidence_s3_key IS NOT NULL AND evidence_hash IS NOT NULL
        ORDER BY created_at DESC
        LIMIT 1
        "#,
        match_id,
        staff_access,
        user_id
    )
    .fetch_optional(db.get_ref())
    .await
    .map_err(ApiError::database_error)?
    .ok_or_else(|| ApiError::not_found("No evidence found for this dispute"))?;

    let key = dispute.evidence_s3_key.as_deref().unwrap_or_default();
    let bytes = store.get(key).await?;
    if hash_evidence(&bytes) != dispute.evidence_hash.as_deref().unwrap_or_default() {
        return Err(ApiError::ServiceUnavailable(
            "Stored dispute evidence failed its integrity check".to_string(),
        ));
    }
    validate_evidence_size(bytes.len())?;

    sqlx::query!(
        r#"
        INSERT INTO audit_logs (user_id, action, resource_type, resource_id, details)
        VALUES ($1, $2, $3, $4, $5)
        "#,
        user_id,
        "DISPUTE_EVIDENCE_ACCESSED",
        "match_dispute",
        dispute.id,
        serde_json::json!({ "sha256": dispute.evidence_hash }) as _
    )
    .execute(db.get_ref())
    .await
    .map_err(ApiError::database_error)?;

    let content_type = if key.ends_with(".png") {
        "image/png"
    } else if key.ends_with(".jpg") {
        "image/jpeg"
    } else if key.ends_with(".webp") {
        "image/webp"
    } else {
        return Err(ApiError::ServiceUnavailable(
            "Stored dispute evidence has an unsupported file type".to_string(),
        ));
    };

    Ok(HttpResponse::Ok()
        .insert_header(("Cache-Control", "private, no-store"))
        .content_type(content_type)
        .body(bytes))
}

/// GET /api/matches/{id}/dispute/evidence/custody
pub async fn get_evidence_custody(
    req: HttpRequest,
    db: web::Data<DbPool>,
    path: web::Path<Uuid>,
) -> Result<HttpResponse, ApiError> {
    let match_id = path.into_inner();
    let user_id = req
        .user_id()
        .ok_or_else(|| ApiError::unauthorized("Authentication required"))?;
    let staff_access = has_staff_access(&req);
    authorize_evidence_access(db.get_ref(), match_id, user_id, staff_access).await?;

    let records = sqlx::query_as::<_, EvidenceCustodyRecord>(
        r#"
        SELECT v.*
        FROM match_dispute_evidence_versions v
        JOIN match_disputes d ON d.id = v.dispute_id
        WHERE d.match_id = $1 AND ($2 OR d.disputing_player_id = $3)
        ORDER BY v.uploaded_at ASC, v.version ASC
        "#,
    )
    .bind(match_id)
    .bind(staff_access)
    .bind(user_id)
    .fetch_all(db.get_ref())
    .await
    .map_err(ApiError::database_error)?;

    sqlx::query!(
        r#"
        INSERT INTO audit_logs (user_id, action, resource_type, details)
        VALUES ($1, $2, $3, $4)
        "#,
        user_id,
        "DISPUTE_EVIDENCE_CUSTODY_ACCESSED",
        "match_dispute",
        serde_json::json!({ "match_id": match_id, "version_count": records.len() }) as _
    )
    .execute(db.get_ref())
    .await
    .map_err(ApiError::database_error)?;

    Ok(HttpResponse::Ok()
        .insert_header(("Cache-Control", "private, no-store"))
        .json(records))
}

/// PUT /api/matches/{id}/dispute/evidence/legal-hold
pub async fn set_evidence_legal_hold(
    req: HttpRequest,
    db: web::Data<DbPool>,
    path: web::Path<Uuid>,
    body: web::Json<EvidenceLegalHoldRequest>,
) -> Result<HttpResponse, ApiError> {
    let claims = req
        .claims()
        .ok_or_else(|| ApiError::unauthorized("Authentication required"))?;
    if !claims.roles.iter().any(|role| role == "admin" || role == "moderator") {
        return Err(ApiError::forbidden("Moderator or admin access required"));
    }
    let actor_id = req
        .user_id()
        .ok_or_else(|| ApiError::unauthorized("Authentication required"))?;
    let match_id = path.into_inner();

    let mut tx = db.begin().await.map_err(ApiError::database_error)?;
    let dispute = sqlx::query_as!(
        MatchDispute,
        r#"
        UPDATE match_disputes
        SET evidence_legal_hold = $1
        WHERE id = (
            SELECT id FROM match_disputes
            WHERE match_id = $2 AND evidence_s3_key IS NOT NULL
            ORDER BY created_at DESC
            LIMIT 1
            FOR UPDATE
        )
        RETURNING *
        "#,
        body.enabled,
        match_id
    )
    .fetch_optional(&mut *tx)
    .await
    .map_err(ApiError::database_error)?
    .ok_or_else(|| ApiError::not_found("No active evidence found for this match"))?;

    sqlx::query!(
        r#"
        INSERT INTO audit_logs (action, resource_type, resource_id, details)
        VALUES ($1, $2, $3, $4)
        "#,
        "DISPUTE_EVIDENCE_LEGAL_HOLD_UPDATED",
        "match_dispute",
        dispute.id,
        serde_json::json!({ "enabled": body.enabled, "actor_id": actor_id }) as _
    )
    .execute(&mut *tx)
    .await
    .map_err(ApiError::database_error)?;
    tx.commit().await.map_err(ApiError::database_error)?;

    Ok(HttpResponse::Ok().json(DisputeEvidenceResponse { dispute }))
}

async fn authorize_match_player(
    db: &DbPool,
    match_id: Uuid,
    user_id: Uuid,
) -> Result<Match, ApiError> {
    let match_record = sqlx::query_as!(Match, "SELECT * FROM matches WHERE id = $1", match_id)
        .fetch_optional(db)
        .await
        .map_err(ApiError::database_error)?
        .ok_or_else(|| ApiError::not_found("Match not found"))?;

    if match_record.player1_id != user_id && match_record.player2_id != Some(user_id) {
        return Err(ApiError::forbidden("User is not a player in this match"));
    }
    Ok(match_record)
}

fn has_staff_access(req: &HttpRequest) -> bool {
    req.claims().is_some_and(|claims| {
        claims.roles.iter().any(|role| role == "admin" || role == "moderator")
    })
}

async fn authorize_evidence_access(
    db: &DbPool,
    match_id: Uuid,
    user_id: Uuid,
    staff_access: bool,
) -> Result<(), ApiError> {
    if staff_access {
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM matches WHERE id = $1)")
            .bind(match_id)
            .fetch_one(db)
            .await
            .map_err(ApiError::database_error)?;
        return if exists {
            Ok(())
        } else {
            Err(ApiError::not_found("Match not found"))
        };
    }
    authorize_match_player(db, match_id, user_id).await.map(|_| ())
}

fn evidence_download_capability() -> Result<&'static EvidenceDownloadCapability, ApiError> {
    EVIDENCE_DOWNLOAD_CAPABILITY
        .get_or_try_init(EvidenceDownloadCapability::from_env)
        .map_err(|message| ApiError::ServiceUnavailable(message))
}

/// POST /api/matches/{id}/dispute/evidence/download-link
/// Issues a read-only capability scoped to the authenticated viewer and active case.
pub async fn issue_evidence_download_link(
    req: HttpRequest,
    db: web::Data<DbPool>,
    path: web::Path<Uuid>,
) -> Result<HttpResponse, ApiError> {
    let match_id = path.into_inner();
    let user_id = req
        .user_id()
        .ok_or_else(|| ApiError::unauthorized("Authentication required"))?;
    let staff_access = has_staff_access(&req);
    authorize_evidence_access(db.get_ref(), match_id, user_id, staff_access).await?;

    let dispute = sqlx::query_as!(
        MatchDispute,
        r#"
        SELECT * FROM match_disputes
        WHERE match_id = $1
          AND evidence_s3_key IS NOT NULL
          AND evidence_deleted_at IS NULL
          AND status NOT IN (2, 3)
        ORDER BY created_at DESC
        LIMIT 1
        "#,
        match_id
    )
    .fetch_optional(db.get_ref())
    .await
    .map_err(ApiError::database_error)?
    .ok_or_else(|| ApiError::forbidden("Evidence is not available for an active dispute"))?;

    let object_key = dispute
        .evidence_s3_key
        .as_deref()
        .ok_or_else(|| ApiError::not_found("Evidence object not found"))?;
    let capability = evidence_download_capability()?
        .issue(user_id, dispute.id, object_key)
        .map_err(ApiError::bad_request)?;
    sqlx::query(
        r#"
        INSERT INTO audit_logs (user_id, action, resource_type, resource_id, details)
        VALUES ($1, 'DISPUTE_EVIDENCE_DOWNLOAD_LINK_ISSUED', 'match_dispute', $2, $3)
        "#,
    )
    .bind(user_id)
    .bind(dispute.id)
    .bind(serde_json::json!({ "match_id": match_id, "expires_in_seconds": 300 }).to_string())
    .execute(db.get_ref())
    .await
    .map_err(ApiError::database_error)?;

    Ok(HttpResponse::Ok().json(EvidenceDownloadLinkResponse {
        capability,
        expires_in_seconds: 300,
    }))
}

/// POST /api/matches/{id}/dispute/evidence/download
/// Redeems a capability once and rechecks viewer, active case, and object scope.
pub async fn redeem_evidence_download(
    req: HttpRequest,
    db: web::Data<DbPool>,
    store: web::Data<Arc<dyn EvidenceStore>>,
    path: web::Path<Uuid>,
    body: web::Json<RedeemEvidenceDownloadRequest>,
) -> Result<HttpResponse, ApiError> {
    let match_id = path.into_inner();
    let user_id = req
        .user_id()
        .ok_or_else(|| ApiError::unauthorized("Authentication required"))?;
    let claims = evidence_download_capability()?.verify(
        &body.capability,
        chrono::Utc::now().timestamp(),
    )
    .map_err(ApiError::forbidden)?;
    if claims.viewer_id != user_id {
        return Err(ApiError::forbidden("Evidence capability belongs to another viewer"));
    }

    let dispute = sqlx::query_as!(
        MatchDispute,
        r#"
        SELECT * FROM match_disputes
        WHERE id = $1 AND match_id = $2
          AND evidence_s3_key IS NOT NULL
          AND evidence_deleted_at IS NULL
          AND status NOT IN (2, 3)
        "#,
        claims.case_id,
        match_id
    )
    .fetch_optional(db.get_ref())
    .await
    .map_err(ApiError::database_error)?
    .ok_or_else(|| ApiError::forbidden("Evidence case is closed or unavailable"))?;
    if dispute.evidence_s3_key.as_deref() != Some(claims.object_key.as_str()) {
        return Err(ApiError::forbidden("Evidence capability object scope changed"));
    }
    let staff_access = has_staff_access(&req);
    authorize_evidence_access(db.get_ref(), match_id, user_id, staff_access).await?;
    evidence_download_capability()?
        .consume(&claims)
        .map_err(ApiError::forbidden)?;

    let bytes = store.get(&claims.object_key).await?;
    if hash_evidence(&bytes) != dispute.evidence_hash.as_deref().unwrap_or_default() {
        return Err(ApiError::ServiceUnavailable(
            "Stored dispute evidence failed its integrity check".to_string(),
        ));
    }
    validate_evidence_size(bytes.len())?;

    sqlx::query(
        r#"
        INSERT INTO audit_logs (user_id, action, resource_type, resource_id, details)
        VALUES ($1, 'DISPUTE_EVIDENCE_DOWNLOADED_BY_LINK', 'match_dispute', $2, $3)
        "#,
    )
    .bind(user_id)
    .bind(dispute.id)
    .bind(serde_json::json!({ "match_id": match_id }).to_string())
    .execute(db.get_ref())
    .await
    .map_err(ApiError::database_error)?;

    let content_type = if claims.object_key.ends_with(".png") {
        "image/png"
    } else if claims.object_key.ends_with(".jpg") {
        "image/jpeg"
    } else if claims.object_key.ends_with(".webp") {
        "image/webp"
    } else {
        return Err(ApiError::ServiceUnavailable(
            "Stored dispute evidence has an unsupported file type".to_string(),
        ));
    };
    Ok(HttpResponse::Ok()
        .insert_header(("Cache-Control", "private, no-store"))
        .insert_header(("Content-Disposition", "attachment"))
        .content_type(content_type)
        .body(bytes))
}

pub fn configure_routes(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/matches").service(
            web::scope("/{id}/dispute")
                .route("", web::get().to(get_match_dispute))
                .route("/evidence", web::post().to(upload_dispute_evidence))
                .route("/evidence", web::get().to(get_dispute_evidence))
                .route("/evidence/download-link", web::post().to(issue_evidence_download_link))
                .route("/evidence/download", web::post().to(redeem_evidence_download))
                .route("/evidence/custody", web::get().to(get_evidence_custody))
                .route("/evidence/legal-hold", web::put().to(set_evidence_legal_hold)),
        ),
    );
}
