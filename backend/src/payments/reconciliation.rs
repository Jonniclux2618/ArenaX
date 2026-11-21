//! Provider settlement reconciliation.
//!
//! Webhook delivery alone cannot prove that a provider settled every captured
//! payment. This module compares provider statement rows against internal
//! captured payments, matching by a stable provider reference, and reports
//! exceptions without ever mutating financial history.
//!
//! Statement ingestion is replay-safe: rows are keyed by
//! `(provider, provider_reference, statement_id)` so re-ingesting the same file
//! or overlapping rows is idempotent. Uncertain rows are never auto-credited;
//! they surface as exceptions requiring an explicit resolution policy.

use std::collections::HashMap;

/// A single row from a provider settlement statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatementRow {
    /// Identifier of the statement file/batch this row came from.
    pub statement_id: String,
    /// Stable provider reference (payment/transaction id).
    pub provider_reference: String,
    /// Settled amount in minor units (integer, provider currency).
    pub amount_minor: i64,
    /// ISO-4217 currency code.
    pub currency: String,
    /// Settlement timestamp in Unix seconds (UTC).
    pub settled_at: i64,
}

/// An internal captured payment record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedPayment {
    pub provider_reference: String,
    pub amount_minor: i64,
    pub currency: String,
    /// Capture timestamp in Unix seconds (UTC).
    pub captured_at: i64,
}

/// Exception classes reported by reconciliation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exception {
    /// Internal captured payment absent from the statement.
    Missing { provider_reference: String },
    /// Same provider reference appears more than once in the statement.
    Duplicate {
        provider_reference: String,
        occurrences: usize,
    },
    /// Statement amount differs from the internal captured amount.
    AmountMismatch {
        provider_reference: String,
        internal_minor: i64,
        statement_minor: i64,
        currency: String,
    },
    /// Settled after the expected settlement window.
    LateSettled {
        provider_reference: String,
        settled_at: i64,
        expected_by: i64,
    },
}

/// Result of reconciling one statement against internal records.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReconciliationReport {
    pub exceptions: Vec<Exception>,
    /// References that matched cleanly (present, unique, amount-equal, on time).
    pub matched: Vec<String>,
}

/// Replay-safe statement store keyed by `(provider, statement_id, reference)`.
#[derive(Debug, Default)]
pub struct StatementStore {
    rows: HashMap<(String, String, String), StatementRow>,
}

impl StatementStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ingest a statement row. Returns `true` if newly inserted, `false` if the
    /// identical row was already present (replay). Conflicting rows for the same
    /// key are rejected so history is never silently overwritten.
    pub fn ingest(&mut self, provider: &str, row: StatementRow) -> Result<bool, IngestError> {
        let key = (
            provider.to_string(),
            row.statement_id.clone(),
            row.provider_reference.clone(),
        );
        match self.rows.get(&key) {
            Some(existing) if existing == &row => Ok(false),
            Some(_) => Err(IngestError::ConflictingRow {
                provider_reference: row.provider_reference,
            }),
            None => {
                self.rows.insert(key, row);
                Ok(true)
            }
        }
    }

    /// All rows for a provider, in stable order.
    pub fn rows_for(&self, provider: &str) -> Vec<&StatementRow> {
        let mut rows: Vec<&StatementRow> = self
            .rows
            .iter()
            .filter(|((p, _, _), _)| p == provider)
            .map(|(_, r)| r)
            .collect();
        rows.sort_by(|a, b| {
            a.provider_reference
                .cmp(&b.provider_reference)
                .then(a.statement_id.cmp(&b.statement_id))
        });
        rows
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngestError {
    ConflictingRow { provider_reference: String },
}

/// Reconcile provider statement rows against internal captured payments.
///
/// `expected_settlement_window` is the maximum allowed delay (seconds) between
/// capture and settlement before a row is flagged as late-settled.
///
/// This function is pure: it never mutates internal records and never credits
/// uncertain rows. Callers must apply an explicit resolution policy for any
/// reported exception.
pub fn reconcile(
    statement_rows: &[StatementRow],
    captured: &[CapturedPayment],
    expected_settlement_window: i64,
) -> ReconciliationReport {
    let mut report = ReconciliationReport::default();

    // Group statement rows by provider reference to detect duplicates.
    let mut by_reference: HashMap<&str, Vec<&StatementRow>> = HashMap::new();
    for row in statement_rows {
        by_reference
            .entry(row.provider_reference.as_str())
            .or_default()
            .push(row);
    }

    let mut captured_by_ref: HashMap<&str, &CapturedPayment> = HashMap::new();
    for payment in captured {
        captured_by_ref.insert(payment.provider_reference.as_str(), payment);
    }

    // Missing: internal captured but absent from statement.
    for payment in captured {
        if !by_reference.contains_key(payment.provider_reference.as_str()) {
            report.exceptions.push(Exception::Missing {
                provider_reference: payment.provider_reference.clone(),
            });
        }
    }

    // Duplicate / amount-mismatch / late-settled.
    let mut references: Vec<&&str> = by_reference.keys().collect();
    references.sort();
    for reference in references {
        let rows = &by_reference[*reference];
        if rows.len() > 1 {
            report.exceptions.push(Exception::Duplicate {
                provider_reference: reference.to_string(),
                occurrences: rows.len(),
            });
            // Duplicates are uncertain; do not attempt amount/late checks.
            continue;
        }

        let row = rows[0];
        let Some(payment) = captured_by_ref.get(*reference) else {
            // Statement row with no internal capture: uncertain, never credited.
            report.exceptions.push(Exception::Missing {
                provider_reference: reference.to_string(),
            });
            continue;
        };

        let mut clean = true;
        if row.amount_minor != payment.amount_minor || row.currency != payment.currency {
            report.exceptions.push(Exception::AmountMismatch {
                provider_reference: reference.to_string(),
                internal_minor: payment.amount_minor,
                statement_minor: row.amount_minor,
                currency: payment.currency.clone(),
            });
            clean = false;
        }

        let expected_by = payment.captured_at.saturating_add(expected_settlement_window);
        if row.settled_at > expected_by {
            report.exceptions.push(Exception::LateSettled {
                provider_reference: reference.to_string(),
                settled_at: row.settled_at,
                expected_by,
            });
            clean = false;
        }

        if clean {
            report.matched.push(reference.to_string());
        }
    }

    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(statement_id: &str, reference: &str, amount: i64, settled_at: i64) -> StatementRow {
        StatementRow {
            statement_id: statement_id.to_string(),
            provider_reference: reference.to_string(),
            amount_minor: amount,
            currency: "USD".to_string(),
            settled_at,
        }
    }

    fn captured(reference: &str, amount: i64, captured_at: i64) -> CapturedPayment {
        CapturedPayment {
            provider_reference: reference.to_string(),
            amount_minor: amount,
            currency: "USD".to_string(),
            captured_at,
        }
    }

    #[test]
    fn duplicate_statement_ingestion_is_replay_safe() {
        let mut store = StatementStore::new();
        let r = row("stmt-1", "pay_1", 1000, 100);
        assert_eq!(store.ingest("acme", r.clone()), Ok(true));
        assert_eq!(store.ingest("acme", r.clone()), Ok(false));
        assert_eq!(store.rows_for("acme").len(), 1);
    }

    #[test]
    fn conflicting_row_is_rejected() {
        let mut store = StatementStore::new();
        store.ingest("acme", row("stmt-1", "pay_1", 1000, 100)).unwrap();
        let err = store.ingest("acme", row("stmt-1", "pay_1", 999, 100));
        assert!(matches!(err, Err(IngestError::ConflictingRow { .. })));
    }

    #[test]
    fn complete_synthetic_day_with_known_exceptions() {
        let captured = vec![
            captured("pay_ok", 1000, 0),
            captured("pay_missing", 500, 0),
            captured("pay_dup", 700, 0),
            captured("pay_amt", 300, 0),
            captured("pay_late", 200, 0),
        ];
        let rows = vec![
            row("s1", "pay_ok", 1000, 10),
            row("s1", "pay_dup", 700, 10),
            row("s1", "pay_dup", 700, 10),
            row("s1", "pay_amt", 250, 10),
            row("s1", "pay_late", 200, 10_000),
        ];
        let report = reconcile(&rows, &captured, 3600);
        assert!(report.matched.contains(&"pay_ok".to_string()));
        assert!(report.exceptions.iter().any(|e| matches!(e, Exception::Missing { provider_reference } if provider_reference == "pay_missing")));
        assert!(report.exceptions.iter().any(|e| matches!(e, Exception::Duplicate { provider_reference, occurrences } if provider_reference == "pay_dup" && *occurrences == 2)));
        assert!(report.exceptions.iter().any(|e| matches!(e, Exception::AmountMismatch { provider_reference, .. } if provider_reference == "pay_amt")));
        assert!(report.exceptions.iter().any(|e| matches!(e, Exception::LateSettled { provider_reference, .. } if provider_reference == "pay_late")));
    }

    #[test]
    fn unknown_statement_row_is_not_credited() {
        let rows = vec![row("s1", "pay_unknown", 100, 10)];
        let report = reconcile(&rows, &[], 3600);
        assert!(report.matched.is_empty());
        assert!(report.exceptions.iter().any(|e| matches!(e, Exception::Missing { provider_reference } if provider_reference == "pay_unknown")));
    }
}
