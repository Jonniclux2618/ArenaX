# WebSocket Event Documentation

ArenaX uses WebSocket for real-time delivery of match events, balance updates, and notifications.

## Connection

```
wss://api.arenax.gg/ws?token=<access_token>
```

The JWT access token is passed as a query parameter. The connection is rejected with HTTP 401 if the token is missing or invalid.

## Broker distribution and recovery

ArenaX Redis Pub/Sub is the live cross-instance fan-out transport. Each publish
also appends a bounded (1,000-entry), ordered Redis Stream tail so reconnecting
subscribers can recover only from their last delivered sequence. Streams and
Pub/Sub channels use the same monotonic per-match/per-user sequence.

If Redis Pub/Sub or Redis Streams is unavailable:

- HTTP and match state remain authoritative in PostgreSQL; realtime delivery is degraded.
- Pub/Sub subscribers retry with a two-second backoff and do not perform client-side replay automatically.
- Clients detect sequence gaps or broker reconnects, then refetch the authoritative snapshot and adopt its watermark.
- Credential revocation still uses the Redis revocation channel; a Redis outage fails authorization to Redis-backed session checks closed. Revoked sessions are closed or lose privileged capabilities when distribution resumes.

### Heartbeat

The server sends a WebSocket `ping` frame every **5 seconds**. If no `pong` is received within **10 seconds**, the connection is closed. Most WebSocket clients handle ping/pong automatically.

You can also send application-level heartbeats:
```json
{ "type": "ping" }
```
Server responds:
```json
{ "type": "pong" }
```

---

## Channel Subscriptions

On connect, you are automatically subscribed to your own user channel (`user:<your_uuid>`).

### Subscribe to a channel
```json
{ "type": "subscribe", "channel": "match:550e8400-e29b-41d4-a716-446655440000" }
```

### Unsubscribe
```json
{ "type": "unsubscribe", "channel": "match:550e8400-e29b-41d4-a716-446655440000" }
```

### Channel naming

| Pattern | Description |
|---------|-------------|
| `user:{user_id}` | Personal events (balance, notifications, match found) |
| `match:{match_id}` | Events for a specific match |

---

## Server → Client Events

All events are JSON objects with a `type` discriminator field.

### Sequence fields (watermark)

Every server → client event carries a monotonically increasing sequence number for its channel:

```json
{
  "type": "balance_update",
  "channel": "user:550e8400-...",
  "seq": 1042,
  "timestamp": "2026-04-24T12:00:00Z"
}
```

- `seq` is scoped per channel and increases by exactly 1 for each event published to that channel.
- Events are only guaranteed to be retained for a limited window (see [Event retention](#event-retention)). A client that falls behind the retention window cannot be replayed and must fetch a fresh snapshot.

---

## Bootstrap Protocol (snapshot + stream)

REST snapshot endpoints return a `stream` watermark alongside the data. The watermark identifies the last event sequence number reflected in the snapshot, so the client can apply subsequent WebSocket events without gaps or duplicates.

### Snapshot response shape

```json
{
  "data": { "...": "..." },
  "stream": {
    "channel": "user:550e8400-...",
    "seq": 1042
  }
}
```

- `stream.channel` is the channel the watermark applies to.
- `stream.seq` is the sequence number of the last event already included in `data`. Events with `seq <= stream.seq` are already reflected in the snapshot and **must be discarded** by the client.

### Client algorithm

1. Open the WebSocket and subscribe to the channel(s) **before** (or concurrently with) fetching the snapshot, buffering any events received.
2. Fetch the snapshot and read `stream.seq` as the watermark.
3. Apply buffered events with `seq > watermark` in ascending `seq` order. Discard events with `seq <= watermark` (stale) and any event whose `seq` was already applied (duplicate).
4. For each subsequent event, expect `seq === lastAppliedSeq + 1`:
   - **Match** → apply the event and set `lastAppliedSeq = seq`.
   - **Gap** (`seq > lastAppliedSeq + 1`) → a message was missed. Stop applying events, request a replacement snapshot (see below), and re-run the bootstrap from step 2.
   - **Stale/duplicate** (`seq <= lastAppliedSeq`) → discard the event; do not re-apply optimistic updates.

### Requesting a replacement snapshot

When a gap is detected, or when the server reports that the requested sequence has expired, the client must re-bootstrap rather than guessing:

```json
{ "type": "resync", "channel": "user:550e8400-...", "from_seq": 1042 }
```

The server responds with a `resync_required` event if the requested `from_seq` is older than the retention window, after which the client should re-fetch the REST snapshot:

```json
{
  "type": "resync_required",
  "channel": "user:550e8400-...",
  "oldest_retained_seq": 2000,
  "timestamp": "2026-04-24T12:00:00Z"
}
```

> Replacement snapshots are authoritative. Any optimistic updates applied locally since the last snapshot must be reconciled against the replacement snapshot instead of being replayed on top of it.

### Event retention

Events are retained per channel for **5 minutes** (or the last **10,000** events, whichever is smaller). A `resync` request for a `from_seq` older than the oldest retained sequence returns `resync_required`; the client must then fetch a fresh snapshot.

### `balance_update`
Fired when the user's wallet balance changes.
```json
{
  "type": "balance_update",
  "user_id": "550e8400-...",
  "balance_ngn": 500000,
  "balance_arenax_tokens": 1000,
  "balance_xlm": 50000000,
  "timestamp": "2026-04-24T12:00:00Z"
}
```
> Amounts: `balance_ngn` in kobo (÷100 for NGN), `balance_xlm` in stroops (÷10,000,000 for XLM).

### `match_found`
Fired when the matchmaker pairs you with an opponent.
```json
{
  "type": "match_found",
  "match_id": "...",
  "opponent_id": "...",
  "opponent_name": "player_two",
  "game_mode": "ranked",
  "timestamp": "2026-04-24T12:00:00Z"
}
```

### `match_status_change`
Fired on every match state transition.
```json
{
  "type": "match_status_change",
  "match_id": "...",
  "from_status": "CREATED",
  "to_status": "STARTED",
  "timestamp": "2026-04-24T12:00:00Z"
}
```

### `match_completed`
Fired when a match reaches the COMPLETED state.
```json
{
  "type": "match_completed",
  "match_id": "...",
  "winner_id": "...",
  "elo_change": 24,
  "timestamp": "2026-04-24T12:00:00Z"
}
```
> `elo_change` is positive for a win, negative for a loss.

### `match_disputed`
Fired when a dispute is raised on a completed match.
```json
{
  "type": "match_disputed",
  "match_id": "...",
  "reason": "Score mismatch reported by player",
  "timestamp": "2026-04-24T12:00:00Z"
}
```

### `notification`
In-app notification pushed in real time.
```json
{
  "type": "notification",
  "id": "...",
  "title": "Tournament starting soon",
  "body": "Your tournament begins in 5 minutes.",
  "category": "tournament",
  "timestamp": "2026-04-24T12:00:00Z"
}
```

### `resync_required`
Sent in response to a `resync` request whose `from_seq` is older than the retention window. The client must fetch a fresh snapshot.
```json
{
  "type": "resync_required",
  "channel": "user:550e8400-...",
  "oldest_retained_seq": 2000,
  "timestamp": "2026-04-24T12:00:00Z"
}
```

### `error`
Sent when the server cannot process a client message.
```json
{
  "type": "error",
  "message": "Invalid message format"
}
```

---

## Client → Server Messages

### `subscribe`
```json
{ "type": "subscribe", "channel": "match:<uuid>" }
```

### `unsubscribe`
```json
{ "type": "unsubscribe", "channel": "match:<uuid>" }
```

### `ping`
```json
{ "type": "ping" }
```

### `resync`
Request replay from a given sequence number. If `from_seq` is older than the retention window, the server replies with `resync_required`.
```json
{ "type": "resync", "channel": "user:<uuid>", "from_seq": 1042 }
```

---

## JavaScript Example

```js
const token = localStorage.getItem('access_token'); // prefer memory storage
const ws = new WebSocket(`wss://api.arenax.gg/ws?token=${token}`);

ws.onopen = () => {
  console.log('Connected');
  // Subscribe to a match channel
  ws.send(JSON.stringify({ type: 'subscribe', channel: 'match:550e8400-...' }));
};

// lastAppliedSeq is seeded from the REST snapshot's stream.seq watermark.
let lastAppliedSeq = null;

ws.onmessage = ({ data }) => {
  const event = JSON.parse(data);
  if (typeof event.seq === 'number') {
    if (lastAppliedSeq === null) {
      // Snapshot not yet loaded: buffer until bootstrap completes.
      buffer.push(event);
      return;
    }
    if (event.seq <= lastAppliedSeq) return; // stale or duplicate
    if (event.seq > lastAppliedSeq + 1) {
      // Gap detected: request a replacement snapshot.
      ws.send(JSON.stringify({ type: 'resync', channel: event.channel, from_seq: lastAppliedSeq }));
      return;
    }
    lastAppliedSeq = event.seq;
  }
  switch (event.type) {
    case 'match_found':
      console.log('Match found!', event.match_id);
      break;
    case 'balance_update':
      console.log('New balance (NGN):', event.balance_ngn / 100);
      break;
    case 'notification':
      showToast(event.title, event.body);
      break;
    case 'resync_required':
      // Retention expired: re-fetch the REST snapshot and re-bootstrap.
      refetchSnapshot();
      break;
  }
};

ws.onclose = (e) => {
  console.log('Disconnected', e.code, e.reason);
  // Implement exponential back-off reconnect here
};
```

---

## Reconnection Strategy

Implement exponential back-off with jitter:

```js
let retryDelay = 1000;
const MAX_DELAY = 30000;

function connect() {
  const ws = new WebSocket(`wss://api.arenax.gg/ws?token=${getToken()}`);
  ws.onopen = () => { retryDelay = 1000; };
  ws.onclose = () => {
    setTimeout(connect, retryDelay + Math.random() * 500);
    retryDelay = Math.min(retryDelay * 2, MAX_DELAY);
  };
}
```

> If the access token expires while connected, the server will close the connection. Refresh the token before reconnecting.

### Reconnect and re-bootstrap

After a reconnect, the client must not assume its `lastAppliedSeq` is still valid. Re-run the bootstrap protocol: fetch a fresh snapshot, read its `stream.seq` watermark, and discard any buffered events with `seq <= watermark`. If the server cannot replay from `lastAppliedSeq` (retention expired), it responds with `resync_required` and the client fetches a replacement snapshot.
