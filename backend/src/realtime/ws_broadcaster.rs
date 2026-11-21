use crate::realtime::events::{
    channels, CredentialRevoked, DeliverEvent, RealtimeEvent,
};
use crate::auth::jwt_service::CREDENTIAL_REVOCATION_CHANNEL;
use crate::realtime::session_registry::SessionRegistry;
use crate::realtime::user_ws::UserWebSocket;
use actix::Addr;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// Maps session_id to the Actix actor address for WebSocket message delivery.
///
/// Separate from SessionRegistry because it holds `Addr<UserWebSocket>` which
/// is Actix-specific, while SessionRegistry only tracks user→session mappings.
pub struct WsAddressBook {
    inner: RwLock<HashMap<Uuid, Addr<UserWebSocket>>>,
}

impl WsAddressBook {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
        }
    }

    pub fn insert(&self, session_id: Uuid, addr: Addr<UserWebSocket>) {
        self.inner.write().unwrap().insert(session_id, addr);
    }

    pub fn remove(&self, session_id: &Uuid) {
        self.inner.write().unwrap().remove(session_id);
    }

    pub fn get(&self, session_id: &Uuid) -> Option<Addr<UserWebSocket>> {
        self.inner.read().unwrap().get(session_id).cloned()
    }

    pub fn close_user(&self, user_id: Uuid, session_id: Option<&str>) {
        let addresses = self.inner.read().unwrap().clone();
        for (address_session_id, addr) in addresses {
            if let Some(requested) = session_id {
                if Uuid::parse_str(requested).ok() != Some(address_session_id) {
                    continue;
                }
            }
            addr.do_send(CredentialRevoked(user_id, session_id.map(str::to_string)));
        }
    }
}

/// Subscribes to Redis Pub/Sub patterns and delivers events to connected
/// WebSocket actors via the WsAddressBook.
pub struct WsBroadcaster {
    redis_url: String,
    registry: Arc<SessionRegistry>,
    address_book: Arc<WsAddressBook>,
}

impl WsBroadcaster {
    pub fn new(
        redis_url: String,
        registry: Arc<SessionRegistry>,
        address_book: Arc<WsAddressBook>,
    ) -> Self {
        Self {
            redis_url,
            registry,
            address_book,
        }
    }

    /// Start listening to Redis Pub/Sub in background tasks.
    pub fn start(self) -> Vec<JoinHandle<()>> {
        let mut handles = Vec::new();

        // Spawn user channel subscriber
        let redis_url = self.redis_url.clone();
        let registry = self.registry.clone();
        let address_book = self.address_book.clone();
        handles.push(tokio::spawn(async move {
            Self::subscribe_loop(
                &redis_url,
                channels::USER_CHANNEL_PATTERN,
                &registry,
                &address_book,
                Self::route_user_event,
            )
            .await;
        }));

        // Spawn match channel subscriber (for future spectator features)
        let redis_url = self.redis_url.clone();
        let registry = self.registry.clone();
        let address_book = self.address_book.clone();
        handles.push(tokio::spawn(async move {
            Self::subscribe_loop(
                &redis_url,
                channels::MATCH_CHANNEL_PATTERN,
                &registry,
                &address_book,
                Self::route_match_event,
            )
            .await;
        }));

        // Spawn leaderboard channel subscriber (Issue #900). Delta events are
        // routed by explicit subscription, exactly like match channels — a
        // client only receives the boards it asked for.
        let redis_url = self.redis_url.clone();
        let registry = self.registry.clone();
        let address_book = self.address_book.clone();
        handles.push(tokio::spawn(async move {
            Self::subscribe_loop(
                &redis_url,
                channels::LEADERBOARD_CHANNEL_PATTERN,
                &registry,
                &address_book,
                Self::route_match_event,
            )
            .await;
        }));

        // Credential revocation is instance-independent: every backend instance
        // subscribes and closes or removes the matching local WebSocket sessions.
        let redis_url = self.redis_url.clone();
        let registry = self.registry.clone();
        let address_book = self.address_book.clone();
        handles.push(tokio::spawn(async move {
            loop {
                match redis::Client::open(redis_url.as_str()) {
                    Ok(client) => match client.get_async_pubsub().await {
                        Ok(mut pubsub) => {
                            if pubsub.subscribe(CREDENTIAL_REVOCATION_CHANNEL).await.is_ok() {
                                use futures_util::StreamExt;
                                let mut stream = pubsub.on_message();
                                while let Some(msg) = stream.next().await {
                                    let payload: String = match msg.get_payload() {
                                        Ok(payload) => payload,
                                        Err(_) => continue,
                                    };
                                    if let Ok(revocation) =
                                        serde_json::from_str::<crate::auth::jwt_service::CredentialRevocation>(&payload)
                                    {
                                        if let Some(session_id) = revocation.session_id.as_deref() {
                                            if let Some(ws_session_id) =
                                                registry.revoke_auth_session(session_id)
                                            {
                                                address_book.close_user(
                                                    revocation.user_id,
                                                    Some(&ws_session_id.to_string()),
                                                );
                                            }
                                        } else {
                                            registry.revoke_user(&revocation.user_id);
                                            address_book.close_user(revocation.user_id, None);
                                        }
                                    }
                                }
                            }
                        }
                        Err(error) => error!(%error, "Credential revocation Pub/Sub reconnect failed"),
                    },
                    Err(error) => error!(%error, "Credential revocation Redis client failed"),
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }));

        info!("WsBroadcaster started — listening to Redis Pub/Sub");
        handles
    }

    async fn subscribe_loop(
        redis_url: &str,
        pattern: &str,
        registry: &Arc<SessionRegistry>,
        address_book: &Arc<WsAddressBook>,
        route_fn: fn(&str, &RealtimeEvent, &Arc<SessionRegistry>, &Arc<WsAddressBook>),
    ) {
        // Create a dedicated Redis connection for pub/sub
        let client = match redis::Client::open(redis_url) {
            Ok(c) => c,
            Err(e) => {
                error!(error = %e, "Failed to create Redis client for Pub/Sub");
                return;
            }
        };

        loop {
            let mut pubsub = match client.get_async_pubsub().await {
                Ok(ps) => ps,
                Err(e) => {
                    error!(pattern = %pattern, error = %e, "Failed to connect Redis Pub/Sub; retrying");
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    continue;
                }
            };

            if let Err(e) = pubsub.psubscribe(pattern).await {
                error!(pattern = %pattern, error = %e, "Failed to psubscribe; retrying");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                continue;
            }

            info!(pattern = %pattern, "Subscribed to Redis Pub/Sub pattern");

            #[derive(serde::Deserialize)]
            struct SequencedEnvelope {
                #[serde(default)]
                seq: u64,
                event: RealtimeEvent,
            }

            use futures_util::StreamExt;
            let mut stream = pubsub.on_message();
            let mut disconnected = false;
            while let Some(msg) = stream.next().await {
                let channel: String = match msg.get_channel() {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                let payload: String = match msg.get_payload() {
                    Ok(p) => p,
                    Err(_) => continue,
                };

                let event = if let Ok(envelope) =
                    serde_json::from_str::<SequencedEnvelope>(&payload)
                {
                    Some(envelope.event)
                } else {
                    serde_json::from_str::<RealtimeEvent>(&payload).ok()
                };
                if let Some(event) = event {
                    route_fn(&channel, &event, registry, address_book);
                } else {
                    warn!(channel = %channel, "Failed to deserialize Pub/Sub message");
                }
            }
            disconnected = true;
            if disconnected {
                warn!(pattern = %pattern, "Redis Pub/Sub stream ended; reconnecting");
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    }

    fn route_user_event(
        channel: &str,
        event: &RealtimeEvent,
        registry: &Arc<SessionRegistry>,
        address_book: &Arc<WsAddressBook>,
    ) {
        // Channel format: "user:<uuid>"
        let user_id_str = match channel.strip_prefix("user:") {
            Some(id) => id,
            None => return,
        };
        let user_id = match Uuid::parse_str(user_id_str) {
            Ok(id) => id,
            Err(_) => return,
        };

        let sessions = registry.get_sessions(&user_id);
        for session_id in sessions {
            if let Some(addr) = address_book.get(&session_id) {
                addr.do_send(DeliverEvent(event.clone()));
            }
        }
        debug!(user_id = %user_id, "Routed event to user sessions");
    }

    fn route_match_event(
        channel: &str,
        event: &RealtimeEvent,
        registry: &Arc<SessionRegistry>,
        address_book: &Arc<WsAddressBook>,
    ) {
        // Channel format: "match:<uuid>"
        let subscribers = registry.get_subscribers(channel);
        
        for session_id in subscribers {
            if let Some(addr) = address_book.get(&session_id) {
                addr.do_send(DeliverEvent(event.clone()));
            }
        }
        
        debug!(channel = %channel, subscriber_count = %registry.get_subscribers(channel).len(), "Routed event to match subscribers");
    }
}
