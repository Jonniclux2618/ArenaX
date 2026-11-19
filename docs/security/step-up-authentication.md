# Step-Up Authentication

Sensitive wallet and API-key mutations require a proof issued after password
reauthentication. `POST /api/v1/auth/step-up` requires the caller's current
password and an exact operation descriptor:

```json
{
  "action": "wallet.withdraw",
  "currentPassword": "<current account password>",
  "operation": {
    "method": "POST",
    "path": "/api/wallet/withdraw",
    "query": {},
    "body": {
      "amount": "100.00",
      "currency": "NGN",
      "destination": "synthetic-local-destination",
      "payment_method": "bank_transfer"
    }
  }
}
```

The returned HMAC proof expires after two minutes, is tied to the authenticated
bearer token, actor, method, path, query, action, and canonical request body, and
is atomically consumed in Redis on the protected operation. Redis failures deny
the operation. A proof cannot authorize another amount, destination, key, or
action. The Rust withdrawal boundary and TypeScript wallet/API-key routes share
the proof format and secret.

The currently protected actions are `wallet.withdraw`, `wallet.transfer`,
`wallet.escrow.lock`, `wallet.escrow.release`, `wallet.escrow.slash`,
`wallet.encryption.rotate`, `api_key.create`, `api_key.revoke`, and
`api_key.rotate`. The operation body must match the one used to obtain the
proof; use decimal strings for wallet amounts.

Social-only accounts without a local password are denied rather than silently
skipping step-up. They must use the existing password recovery flow to establish
a password before performing a sensitive mutation. There is no factor bypass.
The transient Redis consumption key contains the actor id and proof id and
expires with the proof; it is classified as Account data with a two-minute
retention.
