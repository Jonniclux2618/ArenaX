# Realtime delivery and recovery

Redis Pub/Sub remains the low-latency fan-out path between backend instances.
Before publishing its wake-up, each event is appended to a channel-specific
Redis Stream (`realtime:stream:<channel>`). Each stream is bounded to
approximately 1,000 entries, with Redis Stream IDs providing an ordered cursor
for that channel.
Streams expire seven days after their latest event as well as being length
bounded. The local Compose Redis service enables AOF with `everysec` fsync;
production Redis must provide equivalent persistence if replay across a Redis
process restart is required.

Clients may include the last cursor they received when subscribing:

```json
{"type":"subscribe","channel":"match:<match-id>","cursor":"<stream-id>"}
```

The server authorizes the channel before replaying entries after the supplied
cursor. Live envelopes include the updated `cursor`. If the cursor predates
retained history, the subscription reports an error instructing the client to
fetch an authoritative REST snapshot and resume from a fresh cursor. Missing,
expired, malformed, or future cursors also fail with an explicit recovery
error. Match and leaderboard domain versions remain the consistency check for
their respective streams.

Redis is not the source of truth for match state, balances, or settlements.
Event persistence/fan-out failures are logged and do not roll back an already
committed domain action; clients recover from REST snapshots. The fan-out
subscriber reconnects after Redis disconnects. After reconnect, clients should
resubscribe with their last channel cursor; streams provide bounded recovery
without replaying unbounded history. No realtime event triggers a financial
action on the client.

Redis Stream retention is a seven-day recovery window, not a financial or
match-state ledger. Outages that outlast the retained window require clients to
refetch authoritative state rather than attempting to replay an incomplete
sequence.
