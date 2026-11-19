#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENV_FILE="$ROOT_DIR/.env.local"
COMPOSE_FILE="$ROOT_DIR/docker-compose.local.yml"

usage() {
    printf 'Usage: %s {up|down|status|logs}\n' "$0" >&2
}

if [[ $# -ne 1 ]]; then
    usage
    exit 2
fi

for command_name in docker openssl; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        printf 'Missing prerequisite: %s must be installed and on PATH.\n' "$command_name" >&2
        exit 1
    fi
done

if ! docker compose version >/dev/null 2>&1; then
    printf 'Missing prerequisite: Docker Compose v2 is required.\n' >&2
    exit 1
fi

compose() {
    docker compose --env-file "$ENV_FILE" -f "$COMPOSE_FILE" "$@"
}

case "$1" in
    up)
        if [[ ! -f "$ENV_FILE" ]]; then
            umask 077
            temporary_env="$(mktemp "$ROOT_DIR/.env.local.XXXXXX")"
            trap 'rm -f "$temporary_env"' EXIT
            {
                printf 'DEV_POSTGRES_PASSWORD=%s\n' "$(openssl rand -hex 32)"
                printf 'DEV_MINIO_ROOT_USER=arenax-local\n'
                printf 'DEV_MINIO_ROOT_PASSWORD=%s\n' "$(openssl rand -hex 32)"
                printf 'DEV_RABBITMQ_USER=arenax-local\n'
                printf 'DEV_RABBITMQ_PASSWORD=%s\n' "$(openssl rand -hex 32)"
                printf 'DEV_JWT_SECRET=%s\n' "$(openssl rand -hex 32)"
                printf 'DEV_SENSITIVE_OPERATION_SECRET=%s\n' "$(openssl rand -hex 32)"
            } > "$temporary_env"
            chmod 600 "$temporary_env"
            mv "$temporary_env" "$ENV_FILE"
            trap - EXIT
        fi

        compose up -d --wait --wait-timeout 120
        compose exec -T postgres psql -U arenax -d arenax <<'SQL'
CREATE TABLE IF NOT EXISTS arenax_local_dev_seed (
    seed_id TEXT PRIMARY KEY,
    data JSONB NOT NULL
);
INSERT INTO arenax_local_dev_seed (seed_id, data)
VALUES (
    'synthetic-player-001',
    '{"id":"00000000-0000-4000-8000-000000000001","username":"synthetic-player-001","environment":"local-only"}'
)
ON CONFLICT (seed_id) DO UPDATE SET data = EXCLUDED.data;
SQL
        compose exec -T redis redis-cli SET arenax:local:seed:player \
            '{"id":"00000000-0000-4000-8000-000000000001","username":"synthetic-player-001","environment":"local-only"}'
        printf 'Local dependencies are healthy. Credentials are in the ignored .env.local file.\n'
        ;;
    down|status|logs)
        if [[ ! -f "$ENV_FILE" ]]; then
            printf 'No local environment exists yet; run ./scripts/dev-env.sh up first.\n'
            exit 0
        fi
        case "$1" in
            down) compose down ;;
            status) compose ps ;;
            logs) compose logs --tail=100 -f ;;
        esac
        ;;
    *)
        usage
        exit 2
        ;;
esac
