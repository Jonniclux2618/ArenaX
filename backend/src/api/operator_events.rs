//! Operator diagnostics for quarantined Soroban contract events.
//!
//! Undecodable or unknown event payloads are retained in a bounded quarantine
//! (see `indexer::soroban`) with source coordinates and the decoder version that
//! failed to decode them. This module exposes operator-facing endpoints to
//! inspect the quarantine and to explicitly replay entries after a decoder
//! upgrade. Replays are audited.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::indexer::soroban::{QuarantineEntry, QuarantineReason, SorobanIndexer};

/// Shared state for operator event endpoints.
#[derive(Clone)]
pub struct OperatorEventsState {
    pub indexer: Arc<SorobanIndexer>,
}

/// Query parameters for listing quarantined events.
#[derive(Debug, Deserialize)]
pub struct ListQuarantineQuery {
    /// Optional filter by contract id.
    pub contract_id: Option<String>,
    /// Optional filter by decoder version.
    pub decoder_version: Option<u32>,
    /// Optional filter by reason.
    pub reason: Option<String>,
    /// Maximum number of entries to return (bounded).
    pub limit: Option<usize>,
}

/// A redacted, operator-safe view of a quarantined event.
#[derive(Debug, Serialize)]
pub struct QuarantineView {
    pub id: String,
    pub ledger: u32,
    pub tx_hash: String,
    pub event_index: u32,
    pub contract_id: String,
    pub decoder_version: u32,
    pub reason: String,
    /// Redacted payload preview; sensitive fields are masked.
    pub payload_preview: String,
    pub first_seen_at: i64,
    pub attempts: u32,
    pub replayed: bool,
}

impl From<&QuarantineEntry> for QuarantineView {
    fn from(entry: &QuarantineEntry) -> Self {
        QuarantineView {
            id: entry.id.clone(),
            ledger: entry.ledger,
            tx_hash: entry.tx_hash.clone(),
            event_index: entry.event_index,
            contract_id: entry.contract_id.clone(),
            decoder_version: entry.decoder_version,
            reason: entry.reason.as_str().to_string(),
            payload_preview: redact_payload(&entry.raw_payload),
            first_seen_at: entry.first_seen_at,
            attempts: entry.attempts,
            replayed: entry.replayed,
        }
    }
}

/// Request body for replaying quarantined events after a decoder upgrade.
#[derive(Debug, Deserialize)]
pub struct ReplayRequest {
    /// Operator identity recorded in the audit trail.
    pub operator: String,
    /// Decoder version the operator is replaying with.
    pub decoder_version: u32,
    /// Optional explicit entry ids; when omitted, all entries matching the
    /// current decoder version filter are replayed.
    pub entry_ids: Option<Vec<String>>,
    /// Optional contract id filter when `entry_ids` is omitted.
    pub contract_id: Option<String>,
}

/// Response for a replay request.
#[derive(Debug, Serialize)]
pub struct ReplayResponse {
    pub replayed: usize,
    pub skipped: usize,
    pub decoder_version: u32,
    pub audit_id: String,
}

/// Redact sensitive fields from a raw payload before exposing it to operators.
///
/// We keep the payload shape but mask values for keys that commonly carry
/// secrets or personal data. The full payload remains in the bounded quarantine
/// store for replay; only the diagnostic view is redacted.
pub fn redact_payload(raw: &str) -> String {
    const SENSITIVE_KEYS: &[&str] = &[
        "secret",
        "private_key",
        "seed",
        "signature",
        "auth",
        "token",
        "password",
        "mnemonic",
    ];

    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(pos) = rest.find('"') {
        out.push_str(&rest[..pos]);
        rest = &rest[pos..];
        // Find the closing quote of the key.
        if let Some(end) = rest[1..].find('"') {
            let key = &rest[1..1 + end];
            let after_key = &rest[1 + end + 1..];
            out.push('"');
            out.push_str(key);
            out.push('"');
            if SENSITIVE_KEYS.iter().any(|k| key.eq_ignore_ascii_case(k)) {
                // Mask the value following the colon.
                if let Some(colon) = after_key.find(':') {
                    out.push_str(&after_key[..=colon]);
                    let value_rest = &after_key[colon + 1..];
                    let trimmed = value_rest.trim_start();
                    let leading = value_rest.len() - trimmed.len();
                    out.push_str(&value_rest[..leading]);
                    if trimmed.starts_with('"') {
                        if let Some(vend) = trimmed[1..].find('"') {
                            out.push_str("\"[REDACTED]\"");
                            rest = &trimmed[1 + vend + 1..];
                            continue;
                        }
                    }
                    // Non-string value: mask until next comma or brace.
                    let stop = trimmed
                        .find(|c| c == ',' || c == '}')
                        .unwrap_or(trimmed.len());
                    out.push_str("[REDACTED]");
                    rest = &trimmed[stop..];
                    continue;
                }
            }
            rest = after_key;
            continue;
        }
        out.push_str(rest);
        return out;
    }
    out.push_str(rest);
    out
}

/// Build the operator diagnostics router.
pub fn router(state: OperatorEventsState) -> Router {
    Router::new()
        .route("/operator/events/quarantine", get(list_quarantine))
        .route(
            "/operator/events/quarantine/:id",
            get(get_quarantine_entry),
        )
        .route(
            "/operator/events/quarantine/replay",
            post(replay_quarantine),
        )
        .with_state(state)
}

async fn list_quarantine(
    State(state): State<OperatorEventsState>,
    Query(query): Query<ListQuarantineQuery>,
) -> impl IntoResponse {
    let limit = query.limit.unwrap_or(100).min(500);
    let reason = query
        .reason
        .as_deref()
        .and_then(QuarantineReason::from_str_opt);

    let entries = state.indexer.list_quarantine(
        query.contract_id.as_deref(),
        query.decoder_version,
        reason,
        limit,
    );

    let views: Vec<QuarantineView> = entries.iter().map(QuarantineView::from).collect();
    Json(views)
}

async fn get_quarantine_entry(
    State(state): State<OperatorEventsState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.indexer.get_quarantine(&id) {
        Some(entry) => Json(QuarantineView::from(&entry)).into_response(),
        None => (StatusCode::NOT_FOUND, "quarantine entry not found").into_response(),
    }
}

async fn replay_quarantine(
    State(state): State<OperatorEventsState>,
    Json(req): Json<ReplayRequest>,
) -> impl IntoResponse {
    if req.operator.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "operator is required").into_response();
    }

    let result = state.indexer.replay_quarantine(
        &req.operator,
        req.decoder_version,
        req.entry_ids.as_deref(),
        req.contract_id.as_deref(),
    );

    match result {
        Ok(outcome) => Json(ReplayResponse {
            replayed: outcome.replayed,
            skipped: outcome.skipped,
            decoder_version: req.decoder_version,
            audit_id: outcome.audit_id,
        })
        .into_response(),
        Err(err) => (StatusCode::CONFLICT, err.to_string()).into_response(),
    }
}
