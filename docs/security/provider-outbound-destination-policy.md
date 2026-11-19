# Provider Outbound Destination Policy

The backend's outbound payment and evidence-storage HTTP clients use explicit
destination policies:

- Payment requests may target only `api.paystack.co` or
  `api.flutterwave.com`. DNS results are checked for loopback, private,
  link-local, multicast, unspecified, and reserved addresses, then pinned to
  the validated addresses for the request.
- The evidence-storage client targets only the configured `S3_ENDPOINT`.
  It applies the same DNS checks and pins the resolved addresses. Development
  and preview may use a loopback S3 endpoint for a local MinIO instance;
  production may not. Production S3 endpoints must use HTTPS.
- Redirects are disabled, so provider credentials and signed storage requests
  cannot be forwarded to another origin. Connect and total request timeouts
  are bounded. Payment JSON responses are limited to 1 MiB; evidence storage
  reads are limited to the evidence upload limit.
- `S3_ENDPOINT` must not contain URL credentials, a query, or a fragment.
  Provider URLs are fixed in code; user-supplied values are data, never hosts.

Network egress controls should independently allow only the configured S3
service and payment provider hosts. A runtime destination check is defense in
depth, not a replacement for deployment-level egress restrictions.
