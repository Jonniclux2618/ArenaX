use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;

use crate::models::match_model::{Match, MatchState};

/// Errors surfaced by the canonical match service boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchServiceError {
    NotFound,
    StaleVersion { expected: u64, actual: u64 },
    ForbiddenTransition { from: MatchState, to: MatchState },
    FinalizedImmutable,
    MissingReason,
    Unauthorized,
    DuplicateRepair,
}

/// Permission required to issue a repair command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairPermission {
    SupportEngineer,
    Admin,
}

impl RepairPermission {
    fn can_repair(self) -> bool {
        matches!(self, RepairPermission::SupportEngineer | RepairPermission::Admin)
    }
}

/// Actor issuing a repair command.
#[derive(Debug, Clone)]
pub struct RepairActor {
    pub id: String,
    pub permission: RepairPermission,
}

/// A request to repair a stuck match. Requires reason, actor, expected
/// version, and an explicit target transition.
#[derive(Debug, Clone)]
pub struct RepairCommand {
    pub match_id: String,
    pub actor: RepairActor,
    pub reason: String,
    pub expected_version: u64,
    pub target_state: MatchState,
    /// Client-supplied idempotency key so replays are safe.
    pub idempotency_key: String,
}

/// Immutable audit record produced by every successful repair.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepairAuditRecord {
    pub match_id: String,
    pub actor_id: String,
    pub reason: String,
    pub from_state: MatchState,
    pub to_state: MatchState,
    pub from_version: u64,
    pub to_version: u64,
    pub idempotency_key: String,
    pub recorded_at: DateTime<Utc>,
}

/// Allowed repair transitions. Finalized states are terminal and cannot be
/// reopened or double-settled through repair.
fn is_allowed_repair_transition(from: MatchState, to: MatchState) -> bool {
    use MatchState::*;
    match (from, to) {
        (Stuck, InProgress) => true,
        (Stuck, Cancelled) => true,
        (InProgress, Cancelled) => true,
        (InProgress, Completed) => true,
        _ => false,
    }
}

fn is_finalized(state: MatchState) -> bool {
    matches!(state, MatchState::Completed | MatchState::Cancelled)
}

/// Canonical match service. All state mutations, including repairs, flow
/// through here so settlement guards are never bypassed.
pub struct MatchService {
    matches: Mutex<HashMap<String, Match>>,
    audit_log: Mutex<Vec<RepairAuditRecord>>,
    /// idempotency_key -> audit record for replay detection.
    repairs: Mutex<HashMap<String, RepairAuditRecord>>,
}

impl MatchService {
    pub fn new() -> Self {
        Self {
            matches: Mutex::new(HashMap::new()),
            audit_log: Mutex::new(Vec::new()),
            repairs: Mutex::new(HashMap::new()),
        }
    }

    /// Authorized, audited, idempotent match-state repair.
    pub fn repair_match(
        &self,
        cmd: RepairCommand,
    ) -> Result<RepairAuditRecord, MatchServiceError> {
        if !cmd.actor.permission.can_repair() {
            return Err(MatchServiceError::Unauthorized);
        }
        if cmd.reason.trim().is_empty() {
            return Err(MatchServiceError::MissingReason);
        }

        // Replay: return the original immutable audit record unchanged.
        {
            let repairs = self.repairs.lock().expect("repairs lock");
            if let Some(existing) = repairs.get(&cmd.idempotency_key) {
                if existing.match_id == cmd.match_id {
                    return Ok(existing.clone());
                }
                return Err(MatchServiceError::DuplicateRepair);
            }
        }

        let mut matches = self.matches.lock().expect("matches lock");
        let current = matches
            .get(&cmd.match_id)
            .ok_or(MatchServiceError::NotFound)?;

        if current.version != cmd.expected_version {
            return Err(MatchServiceError::StaleVersion {
                expected: cmd.expected_version,
                actual: current.version,
            });
        }

        if is_finalized(current.state) {
            return Err(MatchServiceError::FinalizedImmutable);
        }

        if !is_allowed_repair_transition(current.state, cmd.target_state) {
            return Err(MatchServiceError::ForbiddenTransition {
                from: current.state,
                to: cmd.target_state,
            });
        }

        let from_state = current.state;
        let from_version = current.version;
        let to_version = from_version + 1;

        let record = RepairAuditRecord {
            match_id: cmd.match_id.clone(),
            actor_id: cmd.actor.id.clone(),
            reason: cmd.reason.clone(),
            from_state,
            to_state: cmd.target_state,
            from_version,
            to_version,
            idempotency_key: cmd.idempotency_key.clone(),
            recorded_at: Utc::now(),
        };

        if let Some(m) = matches.get_mut(&cmd.match_id) {
            m.state = cmd.target_state;
            m.version = to_version;
        }

        self.audit_log
            .lock()
            .expect("audit lock")
            .push(record.clone());
        self.repairs
            .lock()
            .expect("repairs lock")
            .insert(cmd.idempotency_key, record.clone());

        Ok(record)
    }

    /// Read-only view of the immutable audit trail.
    pub fn audit_trail(&self) -> Vec<RepairAuditRecord> {
        self.audit_log.lock().expect("audit lock").clone()
    }
}

impl Default for MatchService {
    fn default() -> Self {
        Self::new()
    }
}
