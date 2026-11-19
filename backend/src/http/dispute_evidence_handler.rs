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
use crate::service::match_service::MatchService;
use actix_web::{web, HttpRequest, HttpResponse};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const EVIDENCE_LINK_TTL_SECS: u64 = 5 * 60;

#[derive(Debug, Serialize, Deserialize)]
struct EvidenceLinkClaims {
    match_id: Uuid,
    dispute_id: Uuid,
    viewer_id: Uuid,
    expires_at: u64,
}

#[derive(Debug, Serialize)]
struct EvidenceLinkResponse {
    download_url: String,
    expires_at: u64,
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
    store.put(&s3_key, &content_type, &body).await?;

    let svc = MatchService::new(db.get_ref().clone());
    let dispute = match svc
        .attach_dispute_evidence(match_id, user_id, &hash_hex, &s3_key)
        .await
    {
        Ok(dispute) => dispute,
        Err(error) => {
            let _ = store.delete(&s3_key).await;
            return Err(error);
        }
    };

    sqlx::query!(
        r#"
        INSERT INTO audit_logs (action, resource_type, resource_id, details)
        VALUES ($1, $2, $3, $4)
        "#,
        "DISPUTE_EVIDENCE_UPLOADED",
        "match_dispute",
        dispute.id,
        serde_json::json!({ "sha256": hash_hex, "object_key": s3_key }) as _
    )
    .execute(db.get_ref())
    .await
    .map_err(ApiError::database_error)?;

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

    evidence_response(dispute, store.get_ref().as_ref()).await
}

/// POST /api/matches/{id}/dispute/evidence/download-link
pub async fn create_evidence_download_link(
    req: HttpRequest,
    db: web::Data<DbPool>,
    signing_secret: web::Data<String>,
    path: web::Path<Uuid>,
) -> Result<HttpResponse, ApiError> {
    let match_id = path.into_inner();
    let viewer_id = req
        .user_id()
        .ok_or_else(|| ApiError::unauthorized("Authentication required"))?;
    authorize_match_player(db.get_ref(), match_id, viewer_id).await?;

    let dispute = sqlx::query_as!(
        MatchDispute,
        r#"
        SELECT * FROM match_disputes
        WHERE match_id = $1 AND disputing_player_id = $2
          AND evidence_s3_key IS NOT NULL AND evidence_hash IS NOT NULL
          AND status NOT IN ('resolved', 'rejected', 'closed')
          AND (evidence_retention_until IS NULL OR evidence_retention_until > NOW())
          AND evidence_deleted_at IS NULL
        ORDER BY created_at DESC
        LIMIT 1
        "#,
        match_id,
        viewer_id
    )
    .fetch_optional(db.get_ref())
    .await
    .map_err(ApiError::database_error)?
    .ok_or_else(|| ApiError::not_found("No active evidence found for this match"))?;

    let expires_at = unix_time()?.saturating_add(EVIDENCE_LINK_TTL_SECS);
    let claims = EvidenceLinkClaims {
        match_id,
        dispute_id: dispute.id,
        viewer_id,
        expires_at,
    };
    let payload = serde_json::to_vec(&claims).map_err(|error| {
        ApiError::internal_error(format!("Could not encode evidence link: {error}"))
    })?;
    let payload = URL_SAFE_NO_PAD.encode(payload);
    let mut signer = Hmac::<sha2::Sha256>::new_from_slice(signing_secret.as_bytes())
        .map_err(|error| {
            ApiError::internal_error(format!("Could not initialize evidence link signer: {error}"))
        })?;
    signer.update(payload.as_bytes());
    let token = format!("{payload}.{}", URL_SAFE_NO_PAD.encode(signer.finalize().into_bytes()));

    sqlx::query(
        r#"
        INSERT INTO audit_logs (action, resource_type, resource_id, details)
        VALUES ($1, $2, $3, $4)
        "#,
    )
    .bind("DISPUTE_EVIDENCE_DOWNLOAD_LINK_ISSUED")
    .bind("match_dispute")
    .bind(dispute.id)
    .bind(serde_json::json!({ "viewer_id": viewer_id, "expires_at": expires_at }))
    .execute(db.get_ref())
    .await
    .map_err(ApiError::database_error)?;

    Ok(HttpResponse::Ok()
        .insert_header(("Cache-Control", "private, no-store"))
        .insert_header(("Referrer-Policy", "no-referrer"))
        .json(EvidenceLinkResponse {
            download_url: format!(
                "/api/matches/{match_id}/dispute/evidence/download/{token}"
            ),
            expires_at,
        }))
}

/// GET /api/matches/{id}/dispute/evidence/download/{token}
pub async fn download_evidence_with_link(
    db: web::Data<DbPool>,
    store: web::Data<Arc<dyn EvidenceStore>>,
    signing_secret: web::Data<String>,
    path: web::Path<(Uuid, String)>,
) -> Result<HttpResponse, ApiError> {
    let (match_id, token) = path.into_inner();
    let claims = verify_evidence_link(&token, signing_secret.as_str())?;
    if claims.match_id != match_id || claims.expires_at <= unix_time()? {
        return Err(ApiError::unauthorized("Evidence download link is invalid or expired"));
    }

    authorize_match_player(db.get_ref(), match_id, claims.viewer_id).await?;
    let dispute = sqlx::query_as!(
        MatchDispute,
        r#"
        SELECT * FROM match_disputes
        WHERE id = $1 AND match_id = $2 AND disputing_player_id = $3
          AND evidence_s3_key IS NOT NULL AND evidence_hash IS NOT NULL
          AND status NOT IN ('resolved', 'rejected', 'closed')
          AND (evidence_retention_until IS NULL OR evidence_retention_until > NOW())
          AND evidence_deleted_at IS NULL
        "#,
        claims.dispute_id,
        match_id,
        claims.viewer_id
    )
    .fetch_optional(db.get_ref())
    .await
    .map_err(ApiError::database_error)?
    .ok_or_else(|| ApiError::not_found("Evidence is no longer available"))?;

    sqlx::query(
        r#"
        INSERT INTO audit_logs (action, resource_type, resource_id, details)
        VALUES ($1, $2, $3, $4)
        "#,
    )
    .bind("DISPUTE_EVIDENCE_DOWNLOAD_LINK_USED")
    .bind("match_dispute")
    .bind(dispute.id)
    .bind(serde_json::json!({ "viewer_id": claims.viewer_id }))
    .execute(db.get_ref())
    .await
    .map_err(ApiError::database_error)?;

    evidence_response(dispute, store.get_ref().as_ref()).await
}

async fn evidence_response(
    dispute: MatchDispute,
    store: &dyn EvidenceStore,
) -> Result<HttpResponse, ApiError> {
    let key = dispute.evidence_s3_key.as_deref().unwrap_or_default();
    let bytes = store.get(key).await?;
    if hash_evidence(&bytes) != dispute.evidence_hash.as_deref().unwrap_or_default() {
        return Err(ApiError::ServiceUnavailable(
            "Stored dispute evidence failed its integrity check".to_string(),
        ));
    }
    validate_evidence_size(bytes.len())?;

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
        .insert_header(("Referrer-Policy", "no-referrer"))
        .content_type(content_type)
        .body(bytes))
}

fn unix_time() -> Result<u64, ApiError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|error| ApiError::internal_error(format!("System clock is invalid: {error}")))
}

fn verify_evidence_link(token: &str, signing_secret: &str) -> Result<EvidenceLinkClaims, ApiError> {
    let (payload, signature) = token
        .split_once('.')
        .ok_or_else(|| ApiError::unauthorized("Evidence download link is invalid"))?;
    let signature = URL_SAFE_NO_PAD
        .decode(signature)
        .map_err(|_| ApiError::unauthorized("Evidence download link is invalid"))?;
    let mut verifier = Hmac::<sha2::Sha256>::new_from_slice(signing_secret.as_bytes())
        .map_err(|error| {
            ApiError::internal_error(format!("Could not initialize evidence link verifier: {error}"))
        })?;
    verifier.update(payload.as_bytes());
    verifier
        .verify_slice(&signature)
        .map_err(|_| ApiError::unauthorized("Evidence download link is invalid"))?;
    let payload = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| ApiError::unauthorized("Evidence download link is invalid"))?;
    serde_json::from_slice(&payload)
        .map_err(|_| ApiError::unauthorized("Evidence download link is invalid"))
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

pub fn configure_routes(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/matches").service(
            web::scope("/{id}/dispute")
                .route("", web::get().to(get_match_dispute))
                .route("/evidence", web::post().to(upload_dispute_evidence))
                .route("/evidence", web::get().to(get_dispute_evidence))
                .route(
                    "/evidence/download-link",
                    web::post().to(create_evidence_download_link),
                )
                .route(
                    "/evidence/download/{token}",
                    web::get().to(download_evidence_with_link),
                )
                .route("/evidence/legal-hold", web::put().to(set_evidence_legal_hold)),
        ),
    );
}
