# Match Authority & State Synchronization Service - Implementation Summary

## Overview

The Match Authority Backend Service has been successfully implemented as the central off-chain control plane for all ArenaX competitive activity. It provides temporal authority enforcement, finite state machine-based validation, blockchain coordination, and comprehensive safety guarantees for match operations.

## Architecture

### Core Components

1. **Finite State Machine (FSM)**
   - States: `CREATED`, `STARTED`, `COMPLETED`, `DISPUTED`, `FINALIZED`
   - Enforced transitions with database-level constraints
   - Terminal state protection (FINALIZED cannot transition)
   - Idempotent state transitions

2. **Database Layer** ([migrations/20240930000001_create_match_authority.up.sql](backend/migrations/20240930000001_create_match_authority.up.sql))
   - `match_authority` - Core FSM entity table
   - `match_transitions` - Complete audit trail of state changes
   - `match_chain_sync` - Blockchain transaction tracking
   - `match_reconciliation_log` - Divergence detection between on-chain/off-chain
   - `match_operations` - Idempotency key tracking
   - `match_event_timeline` - Bounded, privacy-aware event timeline (see below)
   - Database triggers for automatic FSM validation
   - Views for monitoring (active matches, pending ops, divergent matches)

3. **Service Layer** ([backend/src/service/match_authority_service.rs](backend/src/service/match_authority_service.rs))
   - `MatchAuthorityService` - Core business logic
   - FSM enforcement before blockchain submission
   - Blockchain integration via SorobanService
   - State transition recording
   - Idempotency guarantees
   - Reconciliation mechanisms
   - Timeline recording at the canonical service boundary

4. **API Layer** ([backend/src/http/match_authority_handler.rs](backend/src/http/match_authority_handler.rs))
   - REST endpoints for match lifecycle operations
   - Request validation with `validator` crate
   - Comprehensive error handling

5. **Real-time Layer** ([backend/src/http/match_ws_handler.rs](backend/src/http/match_ws_handler.rs))
   - WebSocket support for live match updates
   - Subscribe/unsubscribe to match events
   - Heartbeat mechanism for connection health

## Implementation Details

### 1. Finite State Machine

#### States
```rust
pub enum MatchAuthorityState {
    Created,    // Match created on-chain
    Started,    // Match in progress
    Completed,  // Match finished
    Disputed,   // Result contested
    Finalized,  // On-chain settlement complete
}
```

#### Valid Transitions
- `CREATED → STARTED`
- `STARTED → COMPLETED`
- `COMPLETED → DISPUTED | FINALIZED`
- `DISPUTED → FINALIZED`

#### Enforcement
- Application-level validation in Rust
- Database-level triggers for safety
- Transition audit logging

### 2. Data Models ([backend/src/models/match_authority.rs](backend/src/models/match_authority.rs))

```rust
pub struct MatchAuthorityEntity {
    pub id: Uuid,
    pub on_chain_match_id: String,      // bytes32 from Soroban
    pub player_a: String,                // Stellar address
    pub player_b: String,                // Stellar address
    pub winner: Option<String>,
    pub state: MatchAuthorityState,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub last_chain_tx: Option<String>,
    pub idempotency_key: Option<String>,
    pub metadata: serde_json::Value,
}

pub struct MatchTransition {
    pub id: Uuid,
    pub match_id: Uuid,
    pub from_state: MatchAuthorityState,
    pub to_state: MatchAuthorityState,
    pub actor: String,
    pub timestamp: DateTime<Utc>,
    pub chain_tx: Option<String>,
    pub metadata: serde_json::Value,
    pub error: Option<String>,
}
```

### 3. API Endpoints

| Method | Endpoint | Description |
|--------|----------|-------------|
| `POST` | `/api/matches` | Create new match |
| `GET` | `/api/matches/:id` | Get match details + transitions |
| `POST` | `/api/matches/:id/start` | Start match |
| `POST` | `/api/matches/:id/complete` | Complete match with winner |
| `POST` | `/api/matches/:id/dispute` | Raise dispute |
| `POST` | `/api/matches/:id/finalize` | Finalize & settle on-chain |
| `POST` | `/api/matches/:id/reconcile` | Reconcile on-chain/off-chain state |
| `GET` | `/api/matches/:id/timeline` | Authorized, redacted event timeline |
| `GET` | `/api/matches/:id/timeline/export` | Authorized timeline export (support/dispute) |
| `WS` | `/ws/matches/:id` | Subscribe to real-time updates |

### 4. Blockchain Integration

Each state transition triggers a corresponding Soroban contract call:

```rust
// Match Lifecycle Contract Functions
- create_match(player_a, player_b) → match_id
- start_match(match_id)
- complete_match(match_id, winner)
- raise_dispute(match_id, disputer)
- finalize_match(match_id) // Settlement
```

**Transaction Flow:**
1. Validate FSM transition locally
2. Submit transaction to Soroban
3. Persist transaction hash before submission
4. Record pending operation in `match_chain_sync`
5. Update local state
6. Monitor transaction status
7. Record state transition
8. Append a redacted timeline entry (see §7)

### 5. Safety Guarantees

#### Idempotency
- Unique `idempotency_key` prevents duplicate match creation
- Same key returns existing match without re-creating
- Operation tracking in `match_operations` table

#### No Duplicate Match Creation
```sql
CREATE UNIQUE INDEX idx_match_authority_idempotency
ON match_authority(idempotency_key)
WHERE idempotency_key IS NOT NULL;
```

#### No Invalid State Transitions
```sql
CREATE TRIGGER enforce_match_state_transitions
BEFORE UPDATE ON match_authority
FOR EACH ROW
EXECUTE FUNCTION validate_match_state_transition();
```

#### Reconciliation
```rust
pub async fn reconcile_match(&self, match_id: Uuid) -> Result<bool, ApiError> {
    // Fetch on-chain state
    // Compare with off-chain state
    // Log divergence if detected
    // Return synchronization status
}
```

### 6. WebSocket Protocol

#### Client → Server
```json
// Subscribe
{"type": "subscribe", "match_id": "uuid"}

// Unsubscribe
{"type": "unsubscribe", "match_id": "uuid"}

// Ping
{"type": "ping"}
```

#### Server → Client
```json
// State Change
{
  "type": "match_state_changed",
  "match_id": "uuid",
  "from_state": "CREATED",
  "to_state": "STARTED",
  "timestamp": "2024-01-29T10:00:00Z"
}

// Match Completed
{
  "type": "match_completed",
  "match_id": "uuid",
  "winner": "GAAAAA...",
  "completed_at": "2024-01-29T10:00:00Z"
}

// Pong
{"type": "pong"}
```

### 7. Privacy-Aware Match Event Timeline

A finalized score is hard to investigate without a structured record of authoritative state changes. The service persists a **bounded, privacy-aware event timeline** at the canonical service boundary (`MatchAuthorityService`), suitable for support and dispute review.

#### Entry Shape

Each timeline entry references only non-sensitive, authoritative fields:

```rust
pub struct MatchTimelineEntry {
    pub match_id: Uuid,
    pub sequence: i64,          // monotonic per-match ordering
    pub event_type: String,     // e.g. "state_transition", "chain_sync", "reconcile"
    pub actor_class: String,    // "player" | "operator" | "system" | "chain"
    pub timestamp: DateTime<Utc>,
    pub summary: serde_json::Value, // redacted, bounded payload
}
```

- **actor class** — coarse classification only; never raw user identifiers beyond the match participants already stored on the entity.
- **event type** — enumerated authoritative event kinds.
- **sequence** — strictly increasing per match; assigned server-side.
- **timestamp** — server clock at append time.

#### Redaction

Timeline entries **must not** store secrets or unrestricted chat content. The service applies a redaction step before persistence:

- API tokens, session identifiers, and signing material are stripped.
- Free-form chat/message bodies are excluded; only derived, bounded signals (e.g. message counts, moderation flags) may be recorded.
- Payloads are size-bounded; oversized fields are truncated or dropped.

#### Authorization

Timeline read and export are authorized: only the match participants, operators, and dispute reviewers may access a match's timeline. Unauthorized callers receive `403 Forbidden`. Export uses the same authorization gate as read.

#### Retention

Retention is explicit and enforced:

- Timeline entries are retained for a defined window (e.g. 180 days after `FINALIZED`).
- A retention sweep deletes entries past the window; expired entries are not returned by read or export.
- Retention policy is documented alongside the schema.

#### Immutability & Replay Protection

- Finalized match and tournament records are immutable except through an explicit audited correction flow.
- Late or replayed events **cannot rewrite** the finalized record: appends after `FINALIZED` are rejected unless they arrive through the audited correction flow, and sequence numbers prevent out-of-order overwrites.
- Duplicate/replayed events are detected via sequence + idempotency and dropped rather than re-applied.

## Testing

Comprehensive test coverage includes:

1. **FSM Validation Tests** ([backend/src/service/match_authority_service_test.rs](backend/src/service/match_authority_service_test.rs))
   - Valid/invalid transitions
   - Terminal state protection
   - State machine properties

2. **Model Tests** ([backend/src/models/match_authority.rs](backend/src/models/match_authority.rs))
   - Serialization/deserialization
   - DTO validation
   - Entity conversions

3. **Handler Tests** ([backend/src/http/match_authority_handler.rs](backend/src/http/match_authority_handler.rs))
   - Request/response formats
   - Error handling

4. **WebSocket Tests** ([backend/src/http/match_ws_handler.rs](backend/src/http/match_ws_handler.rs))
   - Message serialization
   - Connection lifecycle

5. **Timeline Tests**
   - Ordering (monotonic sequence) for a completed match
   - Redaction (no secrets / chat content persisted)
   - Retention expiry (expired entries excluded from read/export)
   - Export behavior for a completed match
   - Authorization (unauthorized read/export denied)
   - Failure handling and replay/concurrency (late/replayed events cannot rewrite finalized record)

## Database Schema

### Key Tables

**match_authority**
- Stores match entity with FSM state
- Enforces unique on_chain_match_id
- Tracks blockchain transaction hashes
- Supports idempotency keys

**match_transitions**
- Complete audit trail
- Records actor, timestamp, chain_tx
- Stores metadata and errors
- Enables forensics and debugging

**match_event_timeline**
- Bounded, privacy-aware event timeline
- Columns: `match_id`, `sequence`, `event_type`, `actor_class`, `timestamp`, `summary`
- Unique `(match_id, sequence)` prevents replay/overwrite
- Retention window enforced by sweep; expired rows excluded from reads

**match_chain_sync**
- Tracks blockchain operation status
- Supports retry logic
- Records confirmation timestamps
- Error message storage

**match_reconciliation_log**
- Detects divergence
- Logs reconciliation attempts
- Tracks resolution actions
- Enables automated healing

### Database Triggers

1. **validate_match_state_transition()** - Prevents invalid FSM transitions
2. **log_match_state_transition()** - Auto-creates transition records
3. **update_match_authority_timestamp()** - Sets started_at/ended_at automatically

### Database Views

1. **active_matches** - Non-finalized matches with stats
2. **pending_chain_ops** - Operations awaiting chain confirmation
