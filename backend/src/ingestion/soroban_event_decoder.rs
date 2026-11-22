//! Soroban contract event decoding with bounded quarantine for undecodable events.
//!
//! Known-good events decode normally. Unknown topics, invalid XDR, or malformed
//! payloads are retained in a bounded quarantine (with source coordinates and
//! decoder version) so indexing can continue when policy permits, and so an
//! operator can explicitly replay them after a decoder upgrade.

use std::collections::VecDeque;

/// Version of the decoder logic. Bump when decoding semantics change so that
/// quarantined events can be replayed against a newer decoder.
pub const DECODER_VERSION: u32 = 1;

/// Maximum number of quarantined events retained. Oldest entries are evicted
/// first once the bound is reached.
const QUARANTINE_CAPACITY: usize = 1024;

/// Source coordinates identifying where an event came from on-chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventCoordinates {
    pub ledger: u32,
    pub tx_hash: String,
    pub event_index: u32,
    pub contract_id: String,
}

/// A decoded, known-good contract event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedEvent {
    pub coordinates: EventCoordinates,
    pub topic: String,
    pub payload: String,
}

/// Reason an event could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuarantineReason {
    /// Topic is not recognized by this decoder version.
    UnknownTopic(String),
    /// XDR payload could not be parsed.
    InvalidXdr(String),
    /// Payload parsed but did not match the expected shape.
    MalformedPayload(String),
}

/// A quarantined event retained for operator diagnostics and replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantinedEvent {
    pub coordinates: EventCoordinates,
    pub reason: QuarantineReason,
    /// Redacted payload safe for operator diagnostics.
    pub redacted_payload: String,
    /// Decoder version that failed to decode this event.
    pub decoder_version: u32,
    /// Number of replay attempts made against this record.
    pub replay_attempts: u32,
}

/// Policy controlling whether indexing continues past undecodable events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuarantinePolicy {
    /// Quarantine the event and continue processing later events.
    ContinueOnError,
    /// Quarantine the event and halt indexing.
    HaltOnError,
}

/// Outcome of decoding a single event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeOutcome {
    Decoded(DecodedEvent),
    Quarantined(QuarantinedEvent),
}

/// Audit record for an explicit operator-triggered replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayAuditRecord {
    pub coordinates: EventCoordinates,
    pub from_decoder_version: u32,
    pub to_decoder_version: u32,
    pub operator: String,
    pub succeeded: bool,
}

/// Bounded quarantine of undecodable events plus an audit trail of replays.
#[derive(Debug, Default)]
pub struct EventQuarantine {
    entries: VecDeque<QuarantinedEvent>,
    audit: Vec<ReplayAuditRecord>,
}

impl EventQuarantine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Retain a quarantined event, evicting the oldest entry when full.
    pub fn push(&mut self, event: QuarantinedEvent) {
        if self.entries.len() >= QUARANTINE_CAPACITY {
            self.entries.pop_front();
        }
        self.entries.push_back(event);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Snapshot of quarantined events for operator diagnostics.
    pub fn entries(&self) -> Vec<QuarantinedEvent> {
        self.entries.iter().cloned().collect()
    }

    /// Audit trail of explicit replays.
    pub fn audit_trail(&self) -> &[ReplayAuditRecord] {
        &self.audit
    }

    /// Explicitly replay a quarantined event against the current decoder.
    ///
    /// Replay is operator-triggered and always recorded in the audit trail,
    /// whether or not it succeeds. On success the record is removed from the
    /// quarantine and the decoded event is returned.
    pub fn replay(
        &mut self,
        coordinates: &EventCoordinates,
        operator: &str,
        decode: impl Fn(&QuarantinedEvent) -> Result<DecodedEvent, QuarantineReason>,
    ) -> Option<DecodedEvent> {
        let position = self
            .entries
            .iter()
            .position(|e| &e.coordinates == coordinates)?;

        let mut record = self.entries[position].clone();
        record.replay_attempts = record.replay_attempts.saturating_add(1);
        let from_version = record.decoder_version;

        let result = decode(&record);
        let succeeded = result.is_ok();

        self.audit.push(ReplayAuditRecord {
            coordinates: coordinates.clone(),
            from_decoder_version: from_version,
            to_decoder_version: DECODER_VERSION,
            operator: operator.to_string(),
            succeeded,
        });

        match result {
            Ok(decoded) => {
                self.entries.remove(position);
                Some(decoded)
            }
            Err(reason) => {
                record.reason = reason;
                record.decoder_version = DECODER_VERSION;
                self.entries[position] = record;
                None
            }
        }
    }
}

/// Redact sensitive payload fields for operator diagnostics.
///
/// Any `key=value` pair whose key looks sensitive (contains `secret`, `key`,
/// `token`, or `sig`) has its value replaced with `[redacted]`.
pub fn redact_payload(payload: &str) -> String {
    payload
        .split_whitespace()
        .map(|pair| match pair.split_once('=') {
            Some((key, _value)) if is_sensitive_key(key) => format!("{key}=[redacted]"),
            _ => pair.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_sensitive_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    lower.contains("secret")
        || lower.contains("key")
        || lower.contains("token")
        || lower.contains("sig")
}

/// Decode a raw contract event, quarantining it on failure.
///
/// `decode` performs the actual decoding; any error is captured as a
/// quarantine reason. The returned outcome lets the caller decide whether to
/// continue or halt based on [`QuarantinePolicy`].
pub fn decode_or_quarantine(
    coordinates: EventCoordinates,
    raw_payload: &str,
    decode: impl Fn(&EventCoordinates, &str) -> Result<DecodedEvent, QuarantineReason>,
) -> DecodeOutcome {
    match decode(&coordinates, raw_payload) {
        Ok(event) => DecodeOutcome::Decoded(event),
        Err(reason) => DecodeOutcome::Quarantined(QuarantinedEvent {
            coordinates,
            reason,
            redacted_payload: redact_payload(raw_payload),
            decoder_version: DECODER_VERSION,
            replay_attempts: 0,
        }),
    }
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

    fn ok_decode(c: &EventCoordinates, p: &str) -> Result<DecodedEvent, QuarantineReason> {
        Ok(DecodedEvent {
            coordinates: c.clone(),
            topic: "transfer".to_string(),
            payload: p.to_string(),
        })
    }

    #[test]
    fn unknown_topic_is_quarantined() {
        let outcome = decode_or_quarantine(coords(0), "raw", |c, _| {
            Err(QuarantineReason::UnknownTopic("mystery".to_string()))
        });
        match outcome {
            DecodeOutcome::Quarantined(q) => {
                assert_eq!(q.coordinates, coords(0));
                assert_eq!(q.decoder_version, DECODER_VERSION);
            }
            _ => panic!("expected quarantine"),
        }
    }

    #[test]
    fn invalid_xdr_is_quarantined_and_later_events_continue() {
        let mut quarantine = EventQuarantine::new();
        let bad = decode_or_quarantine(coords(0), "bad", |_, _| {
            Err(QuarantineReason::InvalidXdr("bad xdr".to_string()))
        });
        if let DecodeOutcome::Quarantined(q) = bad {
            quarantine.push(q);
        }
        let good = decode_or_quarantine(coords(1), "good", ok_decode);
        assert!(matches!(good, DecodeOutcome::Decoded(_)));
        assert_eq!(quarantine.len(), 1);
    }

    #[test]
    fn quarantine_is_bounded() {
        let mut quarantine = EventQuarantine::new();
        for i in 0..(QUARANTINE_CAPACITY as u32 + 10) {
            quarantine.push(QuarantinedEvent {
                coordinates: coords(i),
                reason: QuarantineReason::MalformedPayload("x".to_string()),
                redacted_payload: String::new(),
                decoder_version: DECODER_VERSION,
                replay_attempts: 0,
            });
        }
        assert_eq!(quarantine.len(), QUARANTINE_CAPACITY);
    }

    #[test]
    fn replay_after_decoder_upgrade_is_audited() {
        let mut quarantine = EventQuarantine::new();
        quarantine.push(QuarantinedEvent {
            coordinates: coords(0),
            reason: QuarantineReason::UnknownTopic("mystery".to_string()),
            redacted_payload: "raw".to_string(),
            decoder_version: DECODER_VERSION - 1,
            replay_attempts: 0,
        });
        let decoded = quarantine.replay(&coords(0), "operator-1", ok_decode);
        assert!(decoded.is_some());
        assert!(quarantine.is_empty());
        let audit = quarantine.audit_trail();
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].operator, "operator-1");
        assert!(audit[0].succeeded);
    }

    #[test]
    fn failed_replay_is_audited_and_retained() {
        let mut quarantine = EventQuarantine::new();
        quarantine.push(QuarantinedEvent {
            coordinates: coords(0),
            reason: QuarantineReason::InvalidXdr("bad".to_string()),
            redacted_payload: "raw".to_string(),
            decoder_version: DECODER_VERSION,
            replay_attempts: 0,
        });
        let decoded = quarantine.replay(&coords(0), "operator-1", |_, _| {
            Err(QuarantineReason::InvalidXdr("still bad".to_string()))
        });
        assert!(decoded.is_none());
        assert_eq!(quarantine.len(), 1);
        assert_eq!(quarantine.entries()[0].replay_attempts, 1);
        assert!(!quarantine.audit_trail()[0].succeeded);
    }

    #[test]
    fn duplicate_delivery_replay_is_idempotent() {
        let mut quarantine = EventQuarantine::new();
        quarantine.push(QuarantinedEvent {
            coordinates: coords(0),
            reason: QuarantineReason::UnknownTopic("mystery".to_string()),
            redacted_payload: "raw".to_string(),
            decoder_version: DECODER_VERSION,
            replay_attempts: 0,
        });
        assert!(quarantine.replay(&coords(0), "op", ok_decode).is_some());
        // Second replay of the same coordinates finds nothing to replay.
        assert!(quarantine.replay(&coords(0), "op", ok_decode).is_none());
        assert_eq!(quarantine.audit_trail().len(), 1);
    }

    #[test]
    fn sensitive_fields_are_redacted() {
        let redacted = redact_payload("amount=10 secret_key=abc token=xyz sig=deadbeef");
        assert_eq!(
            redacted,
            "amount=10 secret_key=[redacted] token=[redacted] sig=[redacted]"
        );
    }
}
