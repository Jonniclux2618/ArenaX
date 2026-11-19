# Reproducible Local Dependencies

Run `./scripts/dev-env.sh up` from the repository root. The command checks for
Docker Compose and OpenSSL, creates a mode-600 ignored `.env.local` on first
use, starts PostgreSQL, Redis, MinIO, and RabbitMQ, waits for all health checks,
and seeds deterministic synthetic records in a dedicated local-only table and
Redis key.

The generated credentials stay in `.env.local`; do not commit or copy them to
deployed environments. The generated `DEV_JWT_SECRET` and
`DEV_SENSITIVE_OPERATION_SECRET` are the shared local values for both
applications; copy them only into ignored local override files or export them
when launching the services. Services bind to localhost. PostgreSQL, Redis,
MinIO, and RabbitMQ are available on ports 5433, 6380, 9000, and 5672
respectively.

`./scripts/dev-env.sh status` shows service state and `./scripts/dev-env.sh logs`
tails the dependency logs. `./scripts/dev-env.sh down` stops the containers
without deleting named volumes or user data. Use Docker Compose directly only
with the same generated env file.
