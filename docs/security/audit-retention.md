# Audit event field policy and retention

Every application audit event must declare a schema containing:

- `required_fields`: fields needed for attribution and investigation.
- `pseudonymized_fields`: identifiers that may be retained only in irreversible,
  keyed or hash-derived form.
- `prohibited_fields`: secrets and raw sensitive values that must never enter
  the audit store or exports.
- `retention_class`: `security` (365 days), `operational` (90 days), or
  `compliance` (seven years).

Redaction occurs before persistence and again before export. Export code must
reject payloads containing prohibited fields; it must not apply a new policy
retroactively to old rows by exposing fields that were restricted by an earlier
schema. Retention is recorded per row and policy changes do not silently alter
the retained period of existing evidence.

Forbidden examples include passwords, API keys, refresh/access tokens, private
keys, full request bodies, raw SSNs/card numbers, deletion targets, and raw
email or phone values. Investigations use opaque identifiers, actor IDs,
resource IDs, action, rejection reason, IP/user-agent context, and timestamps.
