# Security

## Reporting a vulnerability

If you believe you have found a security vulnerability, please report it privately to the maintainers rather than opening a public issue. Include a description of the issue, steps to reproduce, and any relevant context. We will acknowledge receipt and keep you informed of remediation progress.

Do not disclose the vulnerability publicly until a fix has been released and coordinated disclosure has been agreed.

## Security notifications

Security-relevant account events — new device sign-in, credential changes, and payout destination changes — trigger a notification to the account owner. These notifications are designed to be tamper-resistant and actionable:

- **Minimal content.** Notifications contain only what is needed to recognize the event. They never include full email addresses, wallet addresses, IP addresses, or transaction data. Sensitive identifiers are masked (for example, `a***@example.com` or a truncated wallet address) so that a leaked or forwarded notification preview does not expose account details.
- **Safe account activity link.** Each notification links to the account activity page using a relative, first-party path. Links are never built from user-supplied or event-supplied URLs, so a crafted event cannot redirect the recipient to an attacker-controlled destination.
- **Explicit recovery actions.** Every security notification offers a supported report/lock action so the recipient can immediately report the activity or lock the account if they do not recognize it.
- **Idempotent delivery.** Delivery is keyed by the security event so retries and duplicate events do not produce duplicate notifications or duplicate side effects. A retried delivery is a no-op once the event has been delivered.
- **Preferences.** Disabling marketing notifications does not suppress required security notifications. Security notifications are not subject to marketing opt-outs, and marketing preferences are never used to bypass security delivery rules.

## Handling a suspected compromise

If you receive a security notification you do not recognize:

1. Use the report/lock action in the notification to lock the account immediately.
2. Rotate your credentials and review active sessions and connected devices.
3. Review payout destinations and revert any changes you did not make.
4. Contact the maintainers through the private reporting channel above.

## Account recovery

Account recovery challenges are time-bounded and rate-limited. A challenge
cannot be replayed after it expires or exhausts its attempts. Successful
high-risk recovery starts a 24-hour cooling period before another recovery
challenge can be completed, and existing refresh-token and password-reset
credentials are invalidated in the same database transaction.

Recovery outcomes are recorded in the account audit trail without exposing
verification codes or other secret values. Existing email/session channels
must receive an explicit notification of a recovery event. Users who did not
authorize a recovery may appeal through the maintainer private reporting
channel and should leave the cooling period in place until the case is
reviewed.

## Contributor guidance

When adding or changing code that emits security notifications:

- Redact sensitive fields at the owning service boundary before the notification is composed, not in the transport layer.
- Key delivery on the security event identifier so retries remain idempotent.
- Keep the account activity link relative and first-party.
- Add focused automated coverage for authorization, failure handling, duplicate events, disabled marketing preferences, and localization.
