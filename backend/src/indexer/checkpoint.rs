//! Finality-aware Soroban event indexer checkpoint model.
//!
//! Each indexed event is keyed by `(network, contract, ledger, transaction, event_position)`
//! so that reprocessing a ledger (retries, duplicate pages, restarts) is idempotent and
//! never duplicates domain effects. The durable checkpoint is advanced atomically with
//! event persistence inside a single transaction.

use std::collections::BTreeMap;

/// Stable identity for a single Soroban event.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EventKey {
    pub network: String,
    pub contract: String,
    pub ledger: u32,
    pub transaction: String,
    pub event_position: u32,
}

impl EventKey {
    pub fn new(
        network: impl Into<String>,
        contract: impl Into<String>,
        ledger: u32,
        transaction: impl Into<String>,
        event_position: u32,
    ) -> Self {
        Self {
            network: network.into(),
            contract: contract.into(),
            ledger,
            transaction: transaction.into(),
            event_position,
        }
    }
}

/// A persisted event together with its finality status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedEvent {
    pub key: EventKey,
    pub payload: Vec<u8>,
    /// `true` once the ledger is considered final (past the confirmation depth).
    pub final: bool,
}

/// Durable checkpoint tracking the last fully processed ledger per network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    pub network: String,
    pub last_ledger: u32,
    /// Ledger hash of `last_ledger`, used to detect stale checkpoints on restart.
    pub last_ledger_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckpointError {
    /// The incoming ledger is older than the durable checkpoint (stale replay).
    StaleCheckpoint { checkpoint: u32, incoming: u32 },
    /// The incoming ledger hash does not match the recorded checkpoint hash.
    HashMismatch { expected: String, actual: String },
    /// A ledger gap was detected; the indexer must backfill before advancing.
    LedgerGap { expected: u32, incoming: u32 },
}

/// In-memory store used by the indexer. In production this is backed by a single
/// database transaction; the semantics here mirror that contract.
#[derive(Debug, Default)]
pub struct CheckpointStore {
    events: BTreeMap<EventKey, IndexedEvent>,
    checkpoints: BTreeMap<String, Checkpoint>,
}

impl CheckpointStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn checkpoint(&self, network: &str) -> Option<&Checkpoint> {
        self.checkpoints.get(network)
    }

    pub fn event(&self, key: &EventKey) -> Option<&IndexedEvent> {
        self.events.get(key)
    }

    pub fn event_count(&self) -> usize {
        self.events.len()
    }

    /// Persist a page of events for `ledger` and advance the checkpoint atomically.
    ///
    /// Reprocessing the same ledger is a no-op for already-seen events (idempotent
    /// upsert keyed by [`EventKey`]) and never advances the checkpoint backwards.
    pub fn apply_ledger(
        &mut self,
        network: &str,
        ledger: u32,
        ledger_hash: &str,
        events: Vec<IndexedEvent>,
    ) -> Result<&Checkpoint, CheckpointError> {
        if let Some(existing) = self.checkpoints.get(network) {
            if ledger < existing.last_ledger {
                return Err(CheckpointError::StaleCheckpoint {
                    checkpoint: existing.last_ledger,
                    incoming: ledger,
                });
            }
            if ledger == existing.last_ledger {
                if existing.last_ledger_hash != ledger_hash {
                    return Err(CheckpointError::HashMismatch {
                        expected: existing.last_ledger_hash.clone(),
                        actual: ledger_hash.to_string(),
                    });
                }
                // Duplicate page for the already-checkpointed ledger: idempotent upsert.
                self.upsert_events(events);
                return Ok(self.checkpoints.get(network).expect("checkpoint exists"));
            }
            if ledger != existing.last_ledger + 1 {
                return Err(CheckpointError::LedgerGap {
                    expected: existing.last_ledger + 1,
                    incoming: ledger,
                });
            }
        }

        // Atomic section: events and checkpoint are committed together.
        self.upsert_events(events);
        self.checkpoints.insert(
            network.to_string(),
            Checkpoint {
                network: network.to_string(),
                last_ledger: ledger,
                last_ledger_hash: ledger_hash.to_string(),
            },
        );
        Ok(self.checkpoints.get(network).expect("checkpoint exists"))
    }

    /// Mark all events at or below `final_ledger` as final. Idempotent.
    pub fn mark_final(&mut self, network: &str, final_ledger: u32) {
        for event in self.events.values_mut() {
            if event.key.network == network && event.key.ledger <= final_ledger {
                event.final = true;
            }
        }
    }

    fn upsert_events(&mut self, events: Vec<IndexedEvent>) {
        for event in events {
            // Idempotent: an existing key is overwritten, never duplicated.
            self.events.insert(event.key.clone(), event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(ledger: u32, tx: &str, pos: u32) -> IndexedEvent {
        IndexedEvent {
            key: EventKey::new("testnet", "CABC", ledger, tx, pos),
            payload: vec![ledger as u8, pos as u8],
            final: false,
        }
    }

    #[test]
    fn advances_checkpoint_with_events() {
        let mut store = CheckpointStore::new();
        store
            .apply_ledger("testnet", 10, "h10", vec![ev(10, "t1", 0), ev(10, "t1", 1)])
            .unwrap();
        assert_eq!(store.event_count(), 2);
        assert_eq!(store.checkpoint("testnet").unwrap().last_ledger, 10);
    }

    #[test]
    fn duplicate_page_is_idempotent() {
        let mut store = CheckpointStore::new();
        let page = vec![ev(10, "t1", 0), ev(10, "t1", 1)];
        store.apply_ledger("testnet", 10, "h10", page.clone()).unwrap();
        store.apply_ledger("testnet", 10, "h10", page).unwrap();
        assert_eq!(store.event_count(), 2);
    }

    #[test]
    fn empty_ledger_still_advances() {
        let mut store = CheckpointStore::new();
        store.apply_ledger("testnet", 5, "h5", vec![]).unwrap();
        assert_eq!(store.checkpoint("testnet").unwrap().last_ledger, 5);
        assert_eq!(store.event_count(), 0);
    }

    #[test]
    fn stale_checkpoint_is_rejected() {
        let mut store = CheckpointStore::new();
        store.apply_ledger("testnet", 10, "h10", vec![]).unwrap();
        let err = store.apply_ledger("testnet", 9, "h9", vec![]).unwrap_err();
        assert_eq!(err, CheckpointError::StaleCheckpoint { checkpoint: 10, incoming: 9 });
    }

    #[test]
    fn ledger_gap_is_rejected() {
        let mut store = CheckpointStore::new();
        store.apply_ledger("testnet", 10, "h10", vec![]).unwrap();
        let err = store.apply_ledger("testnet", 12, "h12", vec![]).unwrap_err();
        assert_eq!(err, CheckpointError::LedgerGap { expected: 11, incoming: 12 });
    }

    #[test]
    fn hash_mismatch_on_same_ledger_is_rejected() {
        let mut store = CheckpointStore::new();
        store.apply_ledger("testnet", 10, "h10", vec![]).unwrap();
        let err = store.apply_ledger("testnet", 10, "other", vec![]).unwrap_err();
        assert!(matches!(err, CheckpointError::HashMismatch { .. }));
    }

    #[test]
    fn marks_final_idempotently() {
        let mut store = CheckpointStore::new();
        store.apply_ledger("testnet", 10, "h10", vec![ev(10, "t1", 0)]).unwrap();
        store.mark_final("testnet", 10);
        store.mark_final("testnet", 10);
        assert!(store.event(&EventKey::new("testnet", "CABC", 10, "t1", 0)).unwrap().final);
    }
}
