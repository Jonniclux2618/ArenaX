use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use uuid::Uuid;

/// Default lifetime for a spectator token. Short-lived by design.
pub const SPECTATOR_TOKEN_TTL: Duration = Duration::from_secs(300);

/// Fields that must never be exposed to spectator connections.
const PRIVATE_MATCH_FIELDS: &[&str] = &[
    "private_notes",
    "player_hand",
    "player_deck",
    "internal_state",
    "chat_private",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchError {
    NotFound,
    SpectatorsDisabled,
    InvalidToken,
    TokenExpired,
    TokenRevoked,
    TokenMatchMismatch,
    PlayerActionForbidden,
}

#[derive(Debug, Clone)]
pub struct Match {
    pub id: Uuid,
    pub host_id: Uuid,
    pub spectators_enabled: bool,
    pub finalized: bool,
    pub public_fields: HashMap<String, String>,
    pub private_fields: HashMap<String, String>,
}

impl Match {
    pub fn new(id: Uuid, host_id: Uuid) -> Self {
        Self {
            id,
            host_id,
            // Opted out by default.
            spectators_enabled: false,
            finalized: false,
            public_fields: HashMap::new(),
            private_fields: HashMap::new(),
        }
    }

    /// Returns the match view safe for spectators: private fields stripped.
    pub fn spectator_view(&self) -> HashMap<String, String> {
        self.public_fields
            .iter()
            .filter(|(k, _)| !PRIVATE_MATCH_FIELDS.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

#[derive(Debug, Clone)]
struct SpectatorSession {
    match_id: Uuid,
    expires_at: Instant,
    revoked: bool,
}

#[derive(Debug, Default)]
pub struct MatchService {
    matches: Mutex<HashMap<Uuid, Match>>,
    spectator_sessions: Mutex<HashMap<String, SpectatorSession>>,
}

impl MatchService {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create_match(&self, host_id: Uuid) -> Match {
        let mut matches = self.matches.lock().unwrap();
        let id = Uuid::new_v4();
        let m = Match::new(id, host_id);
        matches.insert(id, m.clone());
        m
    }

    /// Host opts in or out of spectator access for a single match.
    pub fn set_spectators_enabled(
        &self,
        match_id: Uuid,
        host_id: Uuid,
        enabled: bool,
    ) -> Result<(), MatchError> {
        let mut matches = self.matches.lock().unwrap();
        let m = matches.get_mut(&match_id).ok_or(MatchError::NotFound)?;
        if m.host_id != host_id {
            return Err(MatchError::NotFound);
        }
        if m.finalized {
            return Err(MatchError::NotFound);
        }
        m.spectators_enabled = enabled;
        if !enabled {
            // Opting out revokes any outstanding spectator sessions.
            let mut sessions = self.spectator_sessions.lock().unwrap();
            for session in sessions.values_mut() {
                if session.match_id == match_id {
                    session.revoked = true;
                }
            }
        }
        Ok(())
    }

    /// Issues a short-lived, read-only, single-match spectator token.
    pub fn issue_spectator_token(&self, match_id: Uuid) -> Result<String, MatchError> {
        let matches = self.matches.lock().unwrap();
        let m = matches.get(&match_id).ok_or(MatchError::NotFound)?;
        if !m.spectators_enabled {
            return Err(MatchError::SpectatorsDisabled);
        }
        drop(matches);

        let token = Uuid::new_v4().to_string();
        let mut sessions = self.spectator_sessions.lock().unwrap();
        sessions.insert(
            token.clone(),
            SpectatorSession {
                match_id,
                expires_at: Instant::now() + SPECTATOR_TOKEN_TTL,
                revoked: false,
            },
        );
        Ok(token)
    }

    /// Revokes a spectator token immediately.
    pub fn revoke_spectator_token(&self, token: &str) -> Result<(), MatchError> {
        let mut sessions = self.spectator_sessions.lock().unwrap();
        let session = sessions.get_mut(token).ok_or(MatchError::InvalidToken)?;
        session.revoked = true;
        Ok(())
    }

    /// Validates a spectator token against a specific match, enforcing expiry,
    /// revocation, and match scoping. Also re-checks that the host still has
    /// spectators enabled so a privacy change mid-session is honored.
    fn authorize_spectator(&self, token: &str, match_id: Uuid) -> Result<(), MatchError> {
        let sessions = self.spectator_sessions.lock().unwrap();
        let session = sessions.get(token).ok_or(MatchError::InvalidToken)?;
        if session.match_id != match_id {
            return Err(MatchError::TokenMatchMismatch);
        }
        if session.revoked {
            return Err(MatchError::TokenRevoked);
        }
        if Instant::now() >= session.expires_at {
            return Err(MatchError::TokenExpired);
        }
        drop(sessions);

        let matches = self.matches.lock().unwrap();
        let m = matches.get(&match_id).ok_or(MatchError::NotFound)?;
        if !m.spectators_enabled {
            return Err(MatchError::SpectatorsDisabled);
        }
        Ok(())
    }

    /// Read-only match view for a spectator. Private fields are stripped.
    pub fn spectator_match_view(
        &self,
        token: &str,
        match_id: Uuid,
    ) -> Result<HashMap<String, String>, MatchError> {
        self.authorize_spectator(token, match_id)?;
        let matches = self.matches.lock().unwrap();
        let m = matches.get(&match_id).ok_or(MatchError::NotFound)?;
        Ok(m.spectator_view())
    }

    /// Authorizes an inbound websocket message. Spectator connections may only
    /// send read-only view requests; any player action is rejected.
    pub fn authorize_websocket_message(
        &self,
        token: &str,
        match_id: Uuid,
        message_kind: &str,
    ) -> Result<(), MatchError> {
        self.authorize_spectator(token, match_id)?;
        match message_kind {
            "view" | "subscribe" => Ok(()),
            _ => Err(MatchError::PlayerActionForbidden),
        }
    }
}
