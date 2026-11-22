use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Transaction};
use tracing::{info, warn};

/// A single Soroban contract event observed on a ledger.
///
/// Events are keyed by (network, contract, ledger, transaction, event position)
/// so that reprocessing a ledger is idempotent and never duplicates domain
/// effects.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SorobanEvent {
    pub network: String,
    pub contract_id: String,
    pub ledger: i64,
    pub transaction_hash: String,
    /// Position of the event within the transaction (0-based).
    pub event_index: i32,
    pub topic: String,
    pub payload: serde_json::Value,
    /// Whether the ledger containing this event is considered final.
    pub is_final: bool,
}

/// Durable indexer checkpoint. Tracks the last fully-processed ledger per
/// network so restarts resume from a known-good position.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexerCheckpoint {
    pub network: String,
    pub last_ledger: i64,
    pub last_ledger_hash: String,
}

pub struct SorobanIndexer {
    pool: PgPool,
    network: String,
    /// Number of ledgers behind the chain tip before a ledger is final.
    finality_depth: i64,
}

impl SorobanIndexer {
    pub fn new(pool: PgPool, network: impl Into<String>, finality_depth: i64) -> Self {
        Self {
            pool,
            network: network.into(),
            finality_depth,
        }
    }

    /// Load the durable checkpoint for this network, if any.
    pub async fn load_checkpoint(&self) -> Result<Option<IndexerCheckpoint>> {
        let row = sqlx::query_as::<_, (String, i64, String)>(
            "SELECT network, last_ledger, last_ledger_hash \
             FROM soroban_indexer_checkpoints WHERE network = $1",
        )
        .bind(&self.network)
        .fetch_optional(&self.pool)
        .await
        .context("failed to load indexer checkpoint")?;

        Ok(row.map(|(network, last_ledger, last_ledger_hash)| IndexerCheckpoint {
            network,
            last_ledger,
            last_ledger_hash,
        }))
    }

    /// Persist a batch of events for a single ledger and advance the checkpoint
    /// atomically. Reprocessing the same ledger is safe: events are upserted on
    /// their natural key and the checkpoint only moves forward.
    pub async fn ingest_ledger(
        &self,
        ledger: i64,
        ledger_hash: &str,
        chain_tip: i64,
        events: &[SorobanEvent],
    ) -> Result<()> {
        let is_final = chain_tip - ledger >= self.finality_depth;

        let mut tx = self
            .pool
            .begin()
            .await
            .context("failed to begin indexer transaction")?;

        for event in events {
            self.upsert_event(&mut tx, event, is_final).await?;
        }

        self.advance_checkpoint(&mut tx, ledger, ledger_hash).await?;

        tx.commit()
            .await
            .context("failed to commit indexer transaction")?;

        info!(
            network = %self.network,
            ledger,
            events = events.len(),
            is_final,
            "indexed soroban ledger"
        );

        Ok(())
    }

    async fn upsert_event(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        event: &SorobanEvent,
        is_final: bool,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO soroban_events \
                (network, contract_id, ledger, transaction_hash, event_index, \
                 topic, payload, is_final) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
             ON CONFLICT (network, contract_id, ledger, transaction_hash, event_index) \
             DO UPDATE SET \
                topic = EXCLUDED.topic, \
                payload = EXCLUDED.payload, \
                is_final = soroban_events.is_final OR EXCLUDED.is_final",
        )
        .bind(&event.network)
        .bind(&event.contract_id)
        .bind(event.ledger)
        .bind(&event.transaction_hash)
        .bind(event.event_index)
        .bind(&event.topic)
        .bind(&event.payload)
        .bind(is_final)
        .execute(&mut **tx)
        .await
        .context("failed to upsert soroban event")?;

        Ok(())
    }

    async fn advance_checkpoint(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        ledger: i64,
        ledger_hash: &str,
    ) -> Result<()> {
        // Only advance forward; a stale or replayed ledger must never rewind the
        // checkpoint. This makes recovery from a deliberately stale checkpoint
        // safe.
        sqlx::query(
            "INSERT INTO soroban_indexer_checkpoints \
                (network, last_ledger, last_ledger_hash) \
             VALUES ($1, $2, $3) \
             ON CONFLICT (network) DO UPDATE SET \
                last_ledger = GREATEST(soroban_indexer_checkpoints.last_ledger, EXCLUDED.last_ledger), \
                last_ledger_hash = CASE \
                    WHEN EXCLUDED.last_ledger >= soroban_indexer_checkpoints.last_ledger \
                    THEN EXCLUDED.last_ledger_hash \
                    ELSE soroban_indexer_checkpoints.last_ledger_hash \
                END",
        )
        .bind(&self.network)
        .bind(ledger)
        .bind(ledger_hash)
        .execute(&mut **tx)
        .await
        .context("failed to advance indexer checkpoint")?;

        Ok(())
    }

    /// Mark all events at or below `final_ledger` as final. Called as the chain
    /// tip advances so consumers can distinguish observed from final events.
    pub async fn mark_final(&self, final_ledger: i64) -> Result<u64> {
        let result = sqlx::query(
            "UPDATE soroban_events SET is_final = TRUE \
             WHERE network = $1 AND ledger <= $2 AND is_final = FALSE",
        )
        .bind(&self.network)
        .bind(final_ledger)
        .execute(&self.pool)
        .await
        .context("failed to mark events final")?;

        if result.rows_affected() > 0 {
            warn!(
                network = %self.network,
                final_ledger,
                rows = result.rows_affected(),
                "marked soroban events final"
            );
        }

        Ok(result.rows_affected())
    }
}
