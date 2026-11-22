# Party Realtime Protocol

This document describes the canonical party realtime boundary: how party
websocket clients receive events, how events are sequenced, and how a client
recovers a consistent state after a reconnect.

## Ownership

The party service is the single authoritative writer for party state. All
membership, readiness, and leader changes flow through the party service and
are broadcast from there. Websocket handlers do not write party state; they
only subscribe to the party stream and forward events to connected clients.
There is no second authoritative write path.

## Event sequencing

Every event emitted on a party stream carries a monotonic sequence number
(`seq`) that is scoped to that party. Sequence numbers start at 1 when the
party is created and increase by exactly one per emitted event. Clients use
`seq` to order events and to detect gaps.

Event envelope:

```json
{
  "type": "party.event",
  "partyId": "p_123",
  "seq": 42,
  "event": { "kind": "member.joined", "payload": { "userId": "u_9" } }
}
```

Clients track the last applied `seq` per party. On receiving an event:

- `seq == lastSeq + 1`: apply the event and advance `lastSeq`.
- `seq <= lastSeq`: duplicate or out-of-order delivery; ignore it.
- `seq > lastSeq + 1`: a gap was detected; the client must recover before
  applying any later events.

## Reconnect and replay

On (re)connect a client sends its last known sequence for the party:

```json
{ "type": "party.subscribe", "partyId": "p_123", "lastSeq": 41 }
```

The server responds in one of two ways:

1. **Replay** — if the requested events are still within the retention window,
   the server replays events `lastSeq + 1 .. currentSeq` in order, then resumes
   live delivery. The client applies them normally.
2. **Snapshot fallback** — if `lastSeq` is older than the retention window, or
   the client omits `lastSeq`, the server sends a fresh authorized snapshot of
   the party state before any later events:

   ```json
   { "type": "party.snapshot", "partyId": "p_123", "seq": 57, "state": { "members": [], "readiness": {}, "leaderId": "u_1" } }
   ```

   The snapshot's `seq` becomes the client's new `lastSeq`. Events with
   `seq <= snapshot.seq` are ignored; events after it are applied in order.

A gap detected mid-stream (without a reconnect) is handled the same way: the
client requests recovery and the server returns a snapshot (or replay) before
later events are applied.

## Authorization

Replay and snapshot requests are authorized against current party membership.
A client that is not a member of the party (or whose membership was revoked)
receives an authorization error and no events or snapshot:

```json
{ "type": "party.error", "partyId": "p_123", "code": "forbidden" }
```

Authorization is checked at request time, so a client that lost membership
cannot recover stale state via replay.

## Retention

The party service retains the event log for a bounded window. Once events fall
outside the window they are no longer replayable and clients must fall back to
a snapshot. Retention is an implementation detail of the party service; clients
must always be prepared to receive a snapshot instead of a replay.

## Validation

Focused automated coverage exercises:

- out-of-order delivery (events applied only in `seq` order),
- duplicate delivery (events with `seq <= lastSeq` ignored),
- reconnect after retention expiry (snapshot returned before later events),
- unauthorized replay requests (rejected with `forbidden`).
