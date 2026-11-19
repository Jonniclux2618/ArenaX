# Dispute Evidence Retention

Evidence objects are private and may be retrieved only by a player in the
associated match or an authorized moderator/admin operation. The backend
validates content type, size, and file signature at upload, stores a SHA-256
digest with the dispute, and verifies that digest before returning an object.

Evidence remains available while the dispute is unresolved. On resolution, the
backend sets a 90-day retention deadline. A moderator or administrator may set
or clear a legal hold; held evidence is not deleted by the scheduled retention
sweep. After the deadline and once any hold is cleared, the object is deleted
from private storage. The dispute's SHA-256 digest, resolution, and audit
entries remain in the database after object deletion so the record of retained
evidence integrity is not erased.

The retention worker runs every six hours. Object deletion is idempotent, and
the evidence pointer is cleared only after the storage deletion succeeds.

An authenticated match participant can request a five-minute, read-only
download URL for their own active dispute. Its signed capability is scoped to
one match, dispute, and viewer; use rechecks participant access, dispute state,
retention, and object existence. Closing/resolving the dispute or revoking the
participant's match access invalidates the link immediately. Issuance and use
are recorded in the audit log without recording the capability itself. The
download route uses a route template in application telemetry and returns
`Cache-Control: private, no-store` and `Referrer-Policy: no-referrer`.