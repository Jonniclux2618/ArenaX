//! Deterministic historical backfill for contract event consumers.
//!
//! Backfill replays a bounded ledger range through the *same* projection
//! application path used by live processing, so live and backfill consumers
//! converge to identical state under the same deduplication rules. Progress is
//! persisted per job so a backfill can be safely resumed or cancelled, and the
//! code version that produced the projection is recorded for auditability.

use std::collections::HashSet;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A single contract event as delivered by the Soroban event source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractEvent {
    /// Ledger sequence the event was emitted in.
    pub ledger: u64,
    /// Stable, globally unique event identifier used for deduplication.
    pub event_id: String,
    /// Opaque payload consumed by the projection.
    pub payload: Vec<u8>,
}

/// Lifecycle state of a backfill job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackfillStatus {
    Pending,
    Running,
    Cancelled,
    Completed,
}

/// Persisted progress for a backfill job. This is the durable record that
/// allows a job to be resumed or cancelled without re-applying events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackfillProgress {
    pub job_id: String,
    /// Inclusive lower bound of the captured ledger range.
    pub from_ledger: u64,
    /// Inclusive upper bound of the captured ledger range.
    pub to_ledger: u64,
    /// Next ledger to process. Resuming starts here.
    pub next_ledger: u64,
    /// Code version that produced the projection, recorded for auditability.
    pub code_version: String,
    pub status: BackfillStatus,
    /// Event ids already applied, so resume never double-applies.
    pub applied_event_ids: HashSet<String>,
}

impl BackfillProgress {
    pub fn new(
        job_id: impl Into<String>,
        from_ledger: u64,
        to_ledger: u64,
        code_version: impl Into<String>,
    ) -> Result<Self, BackfillError> {
        if from_ledger > to_ledger {
            return Err(BackfillError::InvalidRange {
                from_ledger,
                to_ledger,
            });
        }
        Ok(Self {
            job_id: job_id.into(),
            from_ledger,
            to_ledger,
            next_ledger: from_ledger,
            code_version: code_version.into(),
            status: BackfillStatus::Pending,
            applied_event_ids: HashSet::new(),
        })
    }

    pub fn is_done(&self) -> bool {
        self.status == BackfillStatus::Completed || self.next_ledger > self.to_ledger
    }
}

#[derive(Debug, Error)]
pub enum BackfillError {
    #[error("invalid ledger range: from {from_ledger} > to {to_ledger}")]
    InvalidRange { from_ledger: u64, to_ledger: u64 },
    #[error("backfill job {0} was cancelled")]
    Cancelled(String),
    #[error("event source failed at ledger {ledger}: {source}")]
    Source { ledger: u64, source: String },
    #[error("projection failed for event {event_id}: {source}")]
    Projection { event_id: String, source: String },
}

/// Durable store for backfill progress. Implementations must persist writes
/// atomically so a crash mid-batch resumes from the last committed ledger.
pub trait BackfillStore {
    fn load(&self, job_id: &str) -> Result<Option<BackfillProgress>, String>;
    fn save(&self, progress: &BackfillProgress) -> Result<(), String>;
}

/// Source of historical contract events for a ledger range.
pub trait EventSource {
    fn fetch(&self, ledger: u64) -> Result<Vec<ContractEvent>, String>;
}

/// The projection boundary shared by live and backfill consumers. Both paths
/// MUST apply events through this trait so deduplication rules are identical.
pub trait Projection {
    /// Returns `true` if the event was newly applied, `false` if it was a
    /// duplicate that was skipped. Must be idempotent per `event_id`.
    fn apply(&mut self, event: &ContractEvent) -> Result<bool, String>;
}

/// Runs a deterministic backfill over the captured ledger range.
///
/// The job is resumable: progress is loaded from the store, and each ledger is
/// committed only after all of its events are applied. Cancellation is honored
/// between ledgers, leaving a consistent, resumable checkpoint.
pub struct BackfillRunner<S, E, P> {
    store: Arc<S>,
    source: Arc<E>,
    projection: P,
}

impl<S, E, P> BackfillRunner<S, E, P>
where
    S: BackfillStore,
    E: EventSource,
    P: Projection,
{
    pub fn new(store: Arc<S>, source: Arc<E>, projection: P) -> Self {
        Self {
            store,
            source,
            projection,
        }
    }

    /// Request cancellation of a job. Safe to call from another task.
    pub fn cancel(&self, job_id: &str) -> Result<(), BackfillError> {
        if let Some(mut progress) = self
            .store
            .load(job_id)
            .map_err(|source| BackfillError::Source { ledger: 0, source })?
        {
            if progress.status != BackfillStatus::Completed {
                progress.status = BackfillStatus::Cancelled;
                self.store
                    .save(&progress)
                    .map_err(|source| BackfillError::Source { ledger: 0, source })?;
            }
        }
        Ok(())
    }

    /// Resume (or start) a backfill job until completion or cancellation.
    pub fn run(&mut self, job_id: &str) -> Result<BackfillProgress, BackfillError> {
        let mut progress = self
            .store
            .load(job_id)
            .map_err(|source| BackfillError::Source { ledger: 0, source })?
            .ok_or_else(|| BackfillError::Source {
                ledger: 0,
                source: format!("unknown backfill job {job_id}"),
            })?;

        if progress.status == BackfillStatus::Cancelled {
            return Err(BackfillError::Cancelled(job_id.to_string()));
        }
        if progress.is_done() {
            progress.status = BackfillStatus::Completed;
            return Ok(progress);
        }

        progress.status = BackfillStatus::Running;
        self.store
            .save(&progress)
            .map_err(|source| BackfillError::Source { ledger: 0, source })?;

        while progress.next_ledger <= progress.to_ledger {
            let ledger = progress.next_ledger;
            let events = self
                .source
                .fetch(ledger)
                .map_err(|source| BackfillError::Source { ledger, source })?;

            for event in &events {
                // Same dedup rule as live processing: skip already-applied ids.
                if progress.applied_event_ids.contains(&event.event_id) {
                    continue;
                }
                let applied = self.projection.apply(event).map_err(|source| {
                    BackfillError::Projection {
                        event_id: event.event_id.clone(),
                        source,
                    }
                })?;
                if applied {
                    progress.applied_event_ids.insert(event.event_id.clone());
                }
            }

            // Commit progress only after the whole ledger is applied so a
            // crash resumes from a consistent checkpoint.
            progress.next_ledger = ledger + 1;
            self.store
                .save(&progress)
                .map_err(|source| BackfillError::Source { ledger, source })?;

            // Honor cancellation between ledgers.
            if let Some(latest) = self
                .store
                .load(job_id)
                .map_err(|source| BackfillError::Source { ledger, source })?
            {
                if latest.status == BackfillStatus::Cancelled {
                    return Err(BackfillError::Cancelled(job_id.to_string()));
                }
            }
        }

        progress.status = BackfillStatus::Completed;
        self.store
            .save(&progress)
            .map_err(|source| BackfillError::Source {
                ledger: progress.to_ledger,
                source,
            })?;
        Ok(progress)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    #[derive(Default)]
    struct MemStore {
        jobs: RefCell<HashMap<String, BackfillProgress>>,
    }

    impl BackfillStore for MemStore {
        fn load(&self, job_id: &str) -> Result<Option<BackfillProgress>, String> {
            Ok(self.jobs.borrow().get(job_id).cloned())
        }
        fn save(&self, progress: &BackfillProgress) -> Result<(), String> {
            self.jobs
                .borrow_mut()
                .insert(progress.job_id.clone(), progress.clone());
            Ok(())
        }
    }

    struct MapSource {
        events: HashMap<u64, Vec<ContractEvent>>,
        fail_at: Option<u64>,
    }

    impl EventSource for MapSource {
        fn fetch(&self, ledger: u64) -> Result<Vec<ContractEvent>, String> {
            if self.fail_at == Some(ledger) {
                return Err("transient source failure".into());
            }
            Ok(self.events.get(&ledger).cloned().unwrap_or_default())
        }
    }

    #[derive(Default)]
    struct CountingProjection {
        applied: Vec<String>,
    }

    impl Projection for CountingProjection {
        fn apply(&mut self, event: &ContractEvent) -> Result<bool, String> {
            if self.applied.contains(&event.event_id) {
                return Ok(false);
            }
            self.applied.push(event.event_id.clone());
            Ok(true)
        }
    }

    fn ev(ledger: u64, id: &str) -> ContractEvent {
        ContractEvent {
            ledger,
            event_id: id.to_string(),
            payload: vec![],
        }
    }

    fn seeded() -> MapSource {
        let mut events = HashMap::new();
        events.insert(10, vec![ev(10, "a"), ev(10, "b")]);
        events.insert(11, vec![ev(11, "c")]);
        events.insert(12, vec![ev(12, "d"), ev(12, "d")]); // duplicate delivery
        MapSource {
            events,
            fail_at: None,
        }
    }

    #[test]
    fn full_replay_matches_live_dedup() {
        let store = Arc::new(MemStore::default());
        store
            .save(&BackfillProgress::new("job", 10, 12, "v1").unwrap())
            .unwrap();
        let mut runner = BackfillRunner::new(store.clone(), Arc::new(seeded()), CountingProjection::default());
        let done = runner.run("job").unwrap();
        assert_eq!(done.status, BackfillStatus::Completed);
        assert_eq!(done.applied_event_ids.len(), 4);
        assert_eq!(done.code_version, "v1");
    }

    #[test]
    fn resumes_after_failure_without_double_apply() {
        let store = Arc::new(MemStore::default());
        store
            .save(&BackfillProgress::new("job", 10, 12, "v1").unwrap())
            .unwrap();
        let mut failing = seeded();
        failing.fail_at = Some(11);
        let mut runner = BackfillRunner::new(store.clone(), Arc::new(failing), CountingProjection::default());
        assert!(matches!(runner.run("job"), Err(BackfillError::Source { ledger: 11, .. })));

        let saved = store.load("job").unwrap().unwrap();
        assert_eq!(saved.next_ledger, 11);
        assert_eq!(saved.applied_event_ids.len(), 2);

        let mut resumed = BackfillRunner::new(store.clone(), Arc::new(seeded()), CountingProjection::default());
        let done = resumed.run("job").unwrap();
        assert_eq!(done.status, BackfillStatus::Completed);
        assert_eq!(done.applied_event_ids.len(), 4);
    }

    #[test]
    fn cancel_stops_and_is_resumable() {
        let store = Arc::new(MemStore::default());
        store
            .save(&BackfillProgress::new("job", 10, 12, "v1").unwrap())
            .unwrap();
        let mut runner = BackfillRunner::new(store.clone(), Arc::new(seeded()), CountingProjection::default());
        runner.cancel("job").unwrap();
        assert!(matches!(runner.run("job"), Err(BackfillError::Cancelled(_))));

        let mut progress = store.load("job").unwrap().unwrap();
        progress.status = BackfillStatus::Pending;
        store.save(&progress).unwrap();
        let done = runner.run("job").unwrap();
        assert_eq!(done.status, BackfillStatus::Completed);
    }

    #[test]
    fn rejects_inverted_range() {
        assert!(BackfillProgress::new("job", 12, 10, "v1").is_err());
    }
}
