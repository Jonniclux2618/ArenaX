//! Bounded quarantine for undecodable Soroban contract events.
//!
//! When the indexer encounters an event whose topic or payload cannot be
//! decoded (unknown event shape, invalid XDR, malformed body), the raw event is
//! retained here with its source coordinates and the decoder version that
//! failed to interpret it. Known-good later events continue to be processed;
//! quarantined events can be replayed explicitly after a decoder upgrade, and
//! every replay is recorded in an audit trail.

use std::collections::VecDeque;

/// Decoder version stamped on every quarantine record so operators can tell
/// which decoder failed and when a replay is warranted.
pub const DECODER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Default upper bound on retained quarantine records.
pub const DEFAULT_QUARANTINE_CAPACITY: usize = 1024;

/// Fields that must never be persisted verbatim in quarantine records or
/// diagnostics. Matched case-insensitively against payload field names.
const SENSITIVE_FIELDS: &[&str] = &[
    "secret",
    "secret_key",
    "private_key",
    "seed",
    "mnemonic",
    "signature",
    "auth_token",
    "api_key",
];

/// Source coordinates identifying exactly where an event came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventCoordinates {
    pub ledger: u32,
    pub tx_hash: String,
    pub event_index: u32,
    pub contract_id: String,
}

/// Why an event could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuarantineReason {
    /// Topic did not match any known event shape.
    UnknownTopic,
    /// Payload was not valid XDR.
    InvalidXdr,
    /// Payload decoded but was structurally malformed.
    MalformedPayload,
}

/// A single quarantined event with redacted payload and decoder provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantineRecord {
    pub coordinates: EventCoordinates,
    pub reason: QuarantineReason,
    pub decoder_version: String,
    /// Redacted payload: sensitive fields replaced with `[REDACTED]`.
    pub payload: String,
    /// Number of replay attempts performed against this record.
    pub replay_attempts: u32,
}

/// Audit entry emitted whenever an operator explicitly replays a record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayAuditEntry {
    pub coordinates: EventCoordinates,
    pub decoder_version: String,
    pub operator: String,
    pub succeeded: bool,
}

/// Bounded FIFO quarantine. When capacity is exceeded the oldest record is
/// evicted so indexing is never halted by a flood of undecodable events.
#[derive(Debug)]
pub struct EventQuarantine {
    capacity: usize,
    records: VecDeque<QuarantineRecord>,
    audit: Vec<ReplayAuditEntry>,
}

impl EventQuarantine {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            records: VecDeque::new(),
            audit: Vec::new(),
        }
    }

    /// Retain an undecodable event. The payload is redacted before storage.
    pub fn quarantine(
        &mut self,
        coordinates: EventCoordinates,
        reason: QuarantineReason,
        raw_payload: &str,
    ) {
        if self.records.len() >= self.capacity {
            self.records.pop_front();
        }
        self.records.push_back(QuarantineRecord {
            coordinates,
            reason,
            decoder_version: DECODER_VERSION.to_string(),
            payload: redact_payload(raw_payload),
            replay_attempts: 0,
        });
    }

    /// Explicit, audited replay of a quarantined record after a decoder
    /// upgrade. `decode` is invoked with the redacted payload; the outcome is
    /// recorded in the audit trail regardless of success.
    pub fn replay<F>(
        &mut self,
        coordinates: &EventCoordinates,
        operator: &str,
        mut decode: F,
    ) -> bool
    where
        F: FnMut(&QuarantineRecord) -> bool,
    {
        let Some(record) = self
            .records
            .iter_mut()
            .find(|r| &r.coordinates == coordinates)
        else {
            return false;
        };

        record.replay_attempts += 1;
        let succeeded = decode(record);
        self.audit.push(ReplayAuditEntry {
            coordinates: record.coordinates.clone(),
            decoder_version: DECODER_VERSION.to_string(),
            operator: operator.to_string(),
            succeeded,
        });

        if succeeded {
            self.records.retain(|r| &r.coordinates != coordinates);
        }
        succeeded
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn records(&self) -> impl Iterator<Item = &QuarantineRecord> {
        self.records.iter()
    }

    pub fn audit_trail(&self) -> &[ReplayAuditEntry] {
        &self.audit
    }
}

/// Replace values of sensitive fields with `[REDACTED]`. Handles simple
/// `key=value` and `"key":"value"` shapes without pulling in a JSON dep.
pub fn redact_payload(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for token in raw.split(|c| c == ',' || c == '&' || c == ';') {
        let trimmed = token.trim();
        let key = trimmed
            .split(|c| c == '=' || c == ':')
            .next()
            .unwrap_or("")
            .trim_matches(|c| c == '"' || c == ' ' || c == '{' || c == '}')
            .to_ascii_lowercase();
        if SENSITIVE_FIELDS.contains(&key.as_str()) {
            out.push_str(&format!("{}=[REDACTED]", key));
        } else {
            out.push_str(trimmed);
        }
        out.push(',');
    }
    out.trim_end_matches(',').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coords(index: u32) -> EventCoordinates {
        EventCoordinates {
            ledger: 100,
            tx_hash: format!("tx{index}"),
            event_index: index,
            contract_id: "CABC".to_string(),
        }
    }

    #[test]
    fn unknown_topic_is_quarantined_with_coordinates() {
        let mut q = EventQuarantine::new(8);
        q.quarantine(coords(0), QuarantineReason::UnknownTopic, "topic=weird");
        assert_eq!(q.len(), 1);
        let rec = q.records().next().unwrap();
        assert_eq!(rec.coordinates, coords(0));
        assert_eq!(rec.decoder_version, DECODER_VERSION);
    }

    #[test]
    fn invalid_xdr_is_retained() {
        let mut q = EventQuarantine::new(8);
        q.quarantine(coords(1), QuarantineReason::InvalidXdr, "\x00\xff");
        assert_eq!(q.records().next().unwrap().reason, QuarantineReason::InvalidXdr);
    }

    #[test]
    fn bounded_eviction_keeps_latest() {
        let mut q = EventQuarantine::new(2);
        for i in 0..5 {
            q.quarantine(coords(i), QuarantineReason::MalformedPayload, "x");
        }
        assert_eq!(q.len(), 2);
        let indices: Vec<u32> = q.records().map(|r| r.coordinates.event_index).collect();
        assert_eq!(indices, vec![3, 4]);
    }

    #[test]
    fn replay_after_decoder_update_is_audited() {
        let mut q = EventQuarantine::new(8);
        q.quarantine(coords(0), QuarantineReason::UnknownTopic, "a=1");
        let ok = q.replay(&coords(0), "operator-1", |_| true);
        assert!(ok);
        assert!(q.is_empty());
        let audit = q.audit_trail();
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].operator, "operator-1");
        assert!(audit[0].succeeded);
    }

    #[test]
    fn failed_replay_retains_record_and_audits() {
        let mut q = EventQuarantine::new(8);
        q.quarantine(coords(0), QuarantineReason::InvalidXdr, "a=1");
        let ok = q.replay(&coords(0), "op", |_| false);
        assert!(!ok);
        assert_eq!(q.len(), 1);
        assert_eq!(q.records().next().unwrap().replay_attempts, 1);
        assert!(!q.audit_trail()[0].succeeded);
    }

    #[test]
    fn duplicate_delivery_is_retained_independently() {
        let mut q = EventQuarantine::new(8);
        q.quarantine(coords(0), QuarantineReason::UnknownTopic, "a=1");
        q.quarantine(coords(0), QuarantineReason::UnknownTopic, "a=1");
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn sensitive_fields_are_redacted() {
        let redacted = redact_payload("secret=abc123,amount=50,private_key=deadbeef");
        assert!(!redacted.contains("abc123"));
        assert!(!redacted.contains("deadbeef"));
        assert!(redacted.contains("amount=50"));
        assert!(redacted.contains("[REDACTED]"));
    }
}
