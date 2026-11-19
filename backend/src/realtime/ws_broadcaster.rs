use crate::realtime::events::{channels, DeliverEvent, PublishedEvent};
use crate::realtime::session_registry::SessionRegistry;
use crate::realtime::user_ws::UserWebSocket;
use actix::Addr;
use redis::aio::ConnectionManager;
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
}

/// Subscribes to Redis Pub/Sub patterns and delivers persisted stream events
/// to connected WebSocket actors via the WsAddressBook.
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

        info!("WsBroadcaster started — listening to Redis Pub/Sub");
        handles
    }

    async fn subscribe_loop(
        redis_url: &str,
        pattern: &str,
        registry: &Arc<SessionRegistry>,
        address_book: &Arc<WsAddressBook>,
        route_fn: fn(&str, &PublishedEvent, &Arc<SessionRegistry>, &Arc<WsAddressBook>),
    ) {
        loop {
            if let Err(error) =
                Self::subscribe_once(redis_url, pattern, registry, address_book, route_fn).await
            {
                error!(pattern = %pattern, error = %error, "Realtime subscriber disconnected");
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }

    async fn subscribe_once(
        redis_url: &str,
        pattern: &str,
        registry: &Arc<SessionRegistry>,
        address_book: &Arc<WsAddressBook>,
        route_fn: fn(&str, &PublishedEvent, &Arc<SessionRegistry>, &Arc<WsAddressBook>),
    ) -> Result<(), String> {
        // Create a dedicated Redis connection for pub/sub
        let client = redis::Client::open(redis_url).map_err(|error| error.to_string())?;
        let mut commands = ConnectionManager::new(client.clone())
            .await
            .map_err(|error| error.to_string())?;
        let mut pubsub = client
            .get_async_pubsub()
            .await
            .map_err(|error| error.to_string())?;
        pubsub
            .psubscribe(pattern)
            .await
            .map_err(|error| error.to_string())?;

        info!(pattern = %pattern, "Subscribed to Redis Pub/Sub pattern");

        use futures_util::StreamExt;
        let mut stream = pubsub.on_message();

        while let Some(msg) = stream.next().await {
            let channel: String = match msg.get_channel() {
                Ok(c) => c,
                Err(error) => {
                    warn!(pattern = %pattern, error = %error, "Invalid Pub/Sub channel");
                    continue;
                }
            };
            let cursor: String = match msg.get_payload() {
                Ok(p) => p,
                Err(error) => {
                    warn!(pattern = %pattern, error = %error, "Invalid Pub/Sub stream cursor");
                    continue;
                }
            };

            match load_published_event(&mut commands, &channel, &cursor).await {
                Ok(Some(published)) => route_fn(&channel, &published, registry, address_book),
                Ok(None) => {
                    warn!(channel = %channel, cursor = %cursor, "Realtime stream entry was trimmed before fan-out");
                }
                Err(error) => {
                    warn!(channel = %channel, cursor = %cursor, error = %error, "Failed to load realtime stream entry");
                }
            }
        }

        Err("Redis Pub/Sub stream ended".to_string())
    }

    fn route_user_event(
        channel: &str,
        event: &PublishedEvent,
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
                addr.do_send(DeliverEvent {
                    event: event.event.clone(),
                    cursor: Some(event.cursor.clone()),
                });
            }
        }

        debug!(user_id = %user_id, "Routed event to user sessions");
    }

    fn route_match_event(
        channel: &str,
        event: &PublishedEvent,
        registry: &Arc<SessionRegistry>,
        address_book: &Arc<WsAddressBook>,
    ) {
        // Channel format: "match:<uuid>"
        let subscribers = registry.get_subscribers(channel);
        
        for session_id in subscribers {
            if let Some(addr) = address_book.get(&session_id) {
                addr.do_send(DeliverEvent {
                    event: event.event.clone(),
                    cursor: Some(event.cursor.clone()),
                });
            }
        }
        debug!(channel = %channel, subscriber_count = %registry.get_subscribers(channel).len(), "Routed event to match subscribers");
    }
}

async fn load_published_event(
    connection: &mut ConnectionManager,
    channel: &str,
    cursor: &str,
) -> Result<Option<PublishedEvent>, String> {
    let rows: Vec<(String, Vec<(String, String)>)> = redis::cmd("XRANGE")
        .arg(format!("realtime:stream:{channel}"))
        .arg(cursor)
        .arg(cursor)
        .query_async(connection)
        .await
        .map_err(|error| error.to_string())?;
    let Some((_, fields)) = rows.into_iter().next() else {
        return Ok(None);
    };
    let payload = fields
        .into_iter()
        .find_map(|(name, value)| (name == "payload").then_some(value))
        .ok_or_else(|| format!("event stream entry {cursor} has no payload"))?;
    let event = serde_json::from_str(&payload)
        .map_err(|error| format!("invalid event stream entry {cursor}: {error}"))?;

    Ok(Some(PublishedEvent {
        cursor: cursor.to_string(),
        event,
    }))
}
