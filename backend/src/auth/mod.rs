use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Default lifetime for a spectator token. Short-lived by design.
pub const SPECTATOR_TOKEN_TTL: Duration = Duration::from_secs(300);

/// A read-only, single-match scoped spectator grant.
#[derive(Debug, Clone)]
pub struct SpectatorSession {
    pub match_id: String,
    pub issued_at: Instant,
    pub expires_at: Instant,
    pub revoked: bool,
}

impl SpectatorSession {
    pub fn is_valid(&self, now: Instant) -> bool {
        !self.revoked && now < self.expires_at
    }
}

/// Tracks per-match spectator opt-in and issued spectator sessions.
#[derive(Default)]
pub struct SpectatorRegistry {
    /// Matches whose host has opted in to spectator access. Default: opted out.
    opted_in: Mutex<HashMap<String, bool>>,
    /// Active spectator sessions keyed by token.
    sessions: Mutex<HashMap<String, SpectatorSession>>,
}

impl SpectatorRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Host opt-in/opt-out. Spectators are disabled by default.
    pub fn set_opt_in(&self, match_id: &str, enabled: bool) {
        let mut opted_in = self.opted_in.lock().unwrap();
        opted_in.insert(match_id.to_string(), enabled);
        if !enabled {
            // Opting out revokes any outstanding spectator sessions for the match.
            let mut sessions = self.sessions.lock().unwrap();
            for session in sessions.values_mut() {
                if session.match_id == match_id {
                    session.revoked = true;
                }
            }
        }
    }

    pub fn is_opted_in(&self, match_id: &str) -> bool {
        self.opted_in
            .lock()
            .unwrap()
            .get(match_id)
            .copied()
            .unwrap_or(false)
    }

    /// Issue a short-lived, read-only spectator token scoped to one match.
    /// Returns `None` when the host has not opted in.
    pub fn issue_token(&self, match_id: &str, token: String) -> Option<SpectatorSession> {
        if !self.is_opted_in(match_id) {
            return None;
        }
        let now = Instant::now();
        let session = SpectatorSession {
            match_id: match_id.to_string(),
            issued_at: now,
            expires_at: now + SPECTATOR_TOKEN_TTL,
            revoked: false,
        };
        self.sessions
            .lock()
            .unwrap()
            .insert(token, session.clone());
        Some(session)
    }

    /// Revoke a single spectator token.
    pub fn revoke_token(&self, token: &str) {
        if let Some(session) = self.sessions.lock().unwrap().get_mut(token) {
            session.revoked = true;
        }
    }

    /// Validate a spectator token for a specific match. Rejects expired,
    /// revoked, mismatched-match, or opted-out tokens.
    pub fn authorize(&self, token: &str, match_id: &str) -> bool {
        if !self.is_opted_in(match_id) {
            return false;
        }
        let sessions = self.sessions.lock().unwrap();
        match sessions.get(token) {
            Some(session) => session.match_id == match_id && session.is_valid(Instant::now()),
            None => false,
        }
    }
}

/// Authorization decision for an inbound websocket message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageAuthorization {
    /// Authenticated player; may invoke player actions.
    Player,
    /// Read-only spectator; may only receive public match state.
    Spectator,
    /// Rejected.
    Denied,
}

/// Classify an inbound websocket message. Spectator connections may never
/// invoke player actions, regardless of the message payload.
pub fn authorize_message(
    registry: &SpectatorRegistry,
    spectator_token: Option<&str>,
    match_id: &str,
    is_player_action: bool,
) -> MessageAuthorization {
    if let Some(token) = spectator_token {
        if registry.authorize(token, match_id) {
            if is_player_action {
                return MessageAuthorization::Denied;
            }
            return MessageAuthorization::Spectator;
        }
        return MessageAuthorization::Denied;
    }
    MessageAuthorization::Player
}

/// Fields that must never be exposed to spectators.
const PRIVATE_FIELDS: &[&str] = &["private_notes", "host_token", "player_credentials", "internal_state"];

/// Filter a match payload for a spectator. Returns `None` when the spectator
/// is not authorized (expired, revoked, opted out, or wrong match).
pub fn filter_for_spectator(
    registry: &SpectatorRegistry,
    token: &str,
    match_id: &str,
    payload: &HashMap<String, String>,
) -> Option<HashMap<String, String>> {
    if !registry.authorize(token, match_id) {
        return None;
    }
    Some(
        payload
            .iter()
            .filter(|(key, _)| !PRIVATE_FIELDS.contains(&key.as_str()))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    )
}
