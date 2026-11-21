// Soroban event consumer: live ingestion plus deterministic historical backfill.
//
// Live and backfill paths share the same deduplication rules (event id + ledger
// sequence) so a full replay converges to the same projection state as live
// processing. Backfill progress is persisted so it can be resumed or cancelled.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tracing::{info, warn};

/// Identifies a single contract event for deduplication purposes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EventKey {
    pub contract_id: String,
    pub ledger_sequence: i64,
    pub event_id: String,
}

/// A retained chain event as delivered by the Soroban RPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractEvent {
    pub contract_id: String,
    pub ledger_sequence: i64,
    pub event_id: String,
    pub payload: serde_json::Value,
}

impl ContractEvent {
    pub fn key(&self) -> EventKey {
        EventKey {
            contract_id: self.contract_id.clone(),
            ledger_sequence: self.ledger_sequence,
            event_id: self.event_id.clone(),
        }
    }
}

/// Persisted backfill job state. Captures the ledger range and code version so
/// a replay is reproducible, and stores progress so it can be resumed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackfillJob {
    pub job_id: String,
    pub contract_id: String,
    pub start_ledger: i64,
    pub end_ledger: i64,
    /// Code version of the consumer that produced this backfill.
    pub code_version: String,
    /// Next ledger to process (inclusive). Resuming starts here.
    pub next_ledger: i64,
    pub status: BackfillStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackfillStatus {
    Pending,
    Running,
    Completed,
    Cancelled,
    Failed,
}

/// Shared deduplication store used by both live and backfill consumers.
/// Applying an event is idempotent: an already-seen key is skipped.
pub struct EventDedup {
    seen: HashSet<EventKey>,
}

impl EventDedup {
    pub fn new() -> Self {
        Self {
            seen: HashSet::new(),
        }
    }

    /// Returns true if the event was newly applied, false if it was a duplicate.
    pub fn apply_once<F>(&mut self, event: &ContractEvent, apply: F) -> bool
    where
        F: FnOnce(&ContractEvent),
    {
        let key = event.key();
        if self.seen.contains(&key) {
            return false;
        }
        apply(event);
        self.seen.insert(key);
        true
    }
}

impl Default for EventDedup {
    fn default() -> Self {
        Self::new()
    }
}

/// Source of retained chain events for a ledger range.
#[async_trait::async_trait]
pub trait EventSource {
    async fn fetch_range(
        &self,
        contract_id: &str,
        start_ledger: i64,
        end_ledger: i64,
    ) -> Result<Vec<ContractEvent>>;
}

/// Persistence for backfill job progress.
#[async_trait::async_trait]
pub trait BackfillStore {
    async fn load(&self, job_id: &str) -> Result<Option<BackfillJob>>;
    async fn save(&self, job: &BackfillJob) -> Result<()>;
}

/// Postgres-backed backfill store.
pub struct PgBackfillStore {
    pool: PgPool,
}

impl PgBackfillStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl BackfillStore for PgBackfillStore {
    async fn load(&self, job_id: &str) -> Result<Option<BackfillJob>> {
        let row: Option<(String, String, i64, i64, String, i64, String)> = sqlx::query_as(
            "SELECT job_id, contract_id, start_ledger, end_ledger, code_version, next_ledger, status \
             FROM soroban_backfill_jobs WHERE job_id = $1",
        )
        .bind(job_id)
        .fetch_optional(&self.pool)
        .await?;

        row.map(|r| {
            let status = match r.6.as_str() {
                "pending" => BackfillStatus::Pending,
                "running" => BackfillStatus::Running,
                "completed" => BackfillStatus::Completed,
                "cancelled" => BackfillStatus::Cancelled,
                "failed" => BackfillStatus::Failed,
                other => return Err(anyhow!("unknown backfill status: {other}")),
            };
            Ok(BackfillJob {
                job_id: r.0,
                contract_id: r.1,
                start_ledger: r.2,
                end_ledger: r.3,
                code_version: r.4,
                next_ledger: r.5,
                status,
            })
        })
        .transpose()
    }

    async fn save(&self, job: &BackfillJob) -> Result<()> {
        let status = match job.status {
            BackfillStatus::Pending => "pending",
            BackfillStatus::Running => "running",
            BackfillStatus::Completed => "completed",
            BackfillStatus::Cancelled => "cancelled",
            BackfillStatus::Failed => "failed",
        };
        sqlx::query(
            "INSERT INTO soroban_backfill_jobs \
             (job_id, contract_id, start_ledger, end_ledger, code_version, next_ledger, status) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) \
             ON CONFLICT (job_id) DO UPDATE SET \
               next_ledger = EXCLUDED.next_ledger, status = EXCLUDED.status",
        )
        .bind(&job.job_id)
        .bind(&job.contract_id)
        .bind(job.start_ledger)
        .bind(job.end_ledger)
        .bind(&job.code_version)
        .bind(job.next_ledger)
        .bind(status)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

/// Runs a deterministic backfill over a ledger range. Progress is persisted
/// after each ledger so the job can be resumed or cancelled safely.
pub struct BackfillRunner<S, D> {
    source: Arc<S>,
    store: Arc<D>,
    dedup: EventDedup,
}

impl<S, D> BackfillRunner<S, D>
where
    S: EventSource + Send + Sync,
    D: BackfillStore + Send + Sync,
{
    pub fn new(source: Arc<S>, store: Arc<D>) -> Self {
        Self {
            source,
            store,
            dedup: EventDedup::new(),
        }
    }

    /// Resume (or start) a backfill job. Returns the final job state.
    /// Cancellation is observed via `is_cancelled` between ledgers.
    pub async fn run<F, C>(&mut self, job_id: &str, mut apply: F, is_cancelled: C) -> Result<BackfillJob>
    where
        F: FnMut(&ContractEvent),
        C: Fn() -> bool,
    {
        let mut job = self
            .store
            .load(job_id)
            .await?
            .ok_or_else(|| anyhow!("backfill job {job_id} not found"))?;

        if matches!(job.status, BackfillStatus::Completed | BackfillStatus::Cancelled) {
            info!(job_id, ?job.status, "backfill already terminal; nothing to do");
            return Ok(job);
        }

        job.status = BackfillStatus::Running;
        self.store.save(&job).await?;

        let mut ledger = job.next_ledger;
        while ledger <= job.end_ledger {
            if is_cancelled() {
                job.status = BackfillStatus::Cancelled;
                self.store.save(&job).await?;
                info!(job_id, next_ledger = ledger, "backfill cancelled");
                return Ok(job);
            }

            let events = match self
                .source
                .fetch_range(&job.contract_id, ledger, ledger)
                .await
            {
                Ok(events) => events,
                Err(err) => {
                    // Persist progress so a retry resumes from this ledger.
                    job.status = BackfillStatus::Failed;
                    job.next_ledger = ledger;
                    self.store.save(&job).await?;
                    warn!(job_id, ledger, error = %err, "backfill fetch failed; resumable");
                    return Err(err);
                }
            };

            for event in &events {
                // Same dedup rules as live processing: duplicates are skipped.
                self.dedup.apply_once(event, |e| apply(e));
            }

            ledger += 1;
            job.next_ledger = ledger;
            self.store.save(&job).await?;
        }

        job.status = BackfillStatus::Completed;
        self.store.save(&job).await?;
        info!(job_id, "backfill completed");
        Ok(job)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    fn ev(ledger: i64, id: &str) -> ContractEvent {
        ContractEvent {
            contract_id: "C1".into(),
            ledger_sequence: ledger,
            event_id: id.into(),
            payload: serde_json::json!({}),
        }
    }

    struct MemSource {
        events: Vec<ContractEvent>,
        fail_on_ledger: Option<i64>,
    }

    #[async_trait::async_trait]
    impl EventSource for MemSource {
        async fn fetch_range(
            &self,
            _contract_id: &str,
            start_ledger: i64,
            end_ledger: i64,
        ) -> Result<Vec<ContractEvent>> {
            if self.fail_on_ledger == Some(start_ledger) {
                return Err(anyhow!("transient rpc failure"));
            }
            Ok(self
                .events
                .iter()
                .filter(|e| e.ledger_sequence >= start_ledger && e.ledger_sequence <= end_ledger)
                .cloned()
                .collect())
        }
    }

    struct MemStore {
        job: Mutex<Option<BackfillJob>>,
    }

    #[async_trait::async_trait]
    impl BackfillStore for MemStore {
        async fn load(&self, _job_id: &str) -> Result<Option<BackfillJob>> {
            Ok(self.job.lock().unwrap().clone())
        }
        async fn save(&self, job: &BackfillJob) -> Result<()> {
            *self.job.lock().unwrap() = Some(job.clone());
            Ok(())
        }
    }

    fn job() -> BackfillJob {
        BackfillJob {
            job_id: "j1".into(),
            contract_id: "C1".into(),
            start_ledger: 1,
            end_ledger: 3,
            code_version: "v1".into(),
            next_ledger: 1,
            status: BackfillStatus::Pending,
        }
    }

    #[tokio::test]
    async fn backfill_applies_all_events_and_completes() {
        let source = Arc::new(MemSource {
            events: vec![ev(1, "a"), ev(2, "b"), ev(3, "c")],
            fail_on_ledger: None,
        });
        let store = Arc::new(MemStore {
            job: Mutex::new(Some(job())),
        });
        let mut runner = BackfillRunner::new(source, store.clone());
        let applied = AtomicUsize::new(0);
        let out = runner
            .run("j1", |_e| {
                applied.fetch_add(1, Ordering::SeqCst);
            }, || false)
            .await
            .unwrap();
        assert_eq!(out.status, BackfillStatus::Completed);
        assert_eq!(applied.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn duplicate_delivery_is_applied_once() {
        let source = Arc::new(MemSource {
            events: vec![ev(1, "a"), ev(1, "a"), ev(2, "b")],
            fail_on_ledger: None,
        });
        let store = Arc::new(MemStore {
            job: Mutex::new(Some(job())),
        });
        let mut runner = BackfillRunner::new(source, store);
        let applied = AtomicUsize::new(0);
        runner
            .run("j1", |_e| {
                applied.fetch_add(1, Ordering::SeqCst);
            }, || false)
            .await
            .unwrap();
        assert_eq!(applied.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn failure_persists_progress_and_resumes() {
        let source = Arc::new(MemSource {
            events: vec![ev(1, "a"), ev(2, "b"), ev(3, "c")],
            fail_on_ledger: Some(2),
        });
        let store = Arc::new(MemStore {
            job: Mutex::new(Some(job())),
        });
        let mut runner = BackfillRunner::new(source, store.clone());
        let err = runner.run("j1", |_e| {}, || false).await;
        assert!(err.is_err());
        let saved = store.load("j1").await.unwrap().unwrap();
        assert_eq!(saved.status, BackfillStatus::Failed);
        assert_eq!(saved.next_ledger, 2);
    }

    #[tokio::test]
    async fn cancellation_stops_and_persists() {
        let source = Arc::new(MemSource {
            events: vec![ev(1, "a"), ev(2, "b")],
            fail_on_ledger: None,
        });
        let store = Arc::new(MemStore {
            job: Mutex::new(Some(job())),
        });
        let mut runner = BackfillRunner::new(source, store.clone());
        let out = runner.run("j1", |_e| {}, || true).await.unwrap();
        assert_eq!(out.status, BackfillStatus::Cancelled);
        let saved = store.load("j1").await.unwrap().unwrap();
        assert_eq!(saved.status, BackfillStatus::Cancelled);
    }
}
