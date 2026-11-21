#!/usr/bin/env sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$ROOT"

fail() {
  printf 'Error: %s\n' "$*" >&2
  exit 1
}

command -v docker >/dev/null 2>&1 || fail 'Docker is required (install Docker Desktop or Docker Engine)'
docker compose version >/dev/null 2>&1 || fail 'Docker Compose v2 is required (`docker compose`)'

ensure_env() {
  if [ ! -f .env.local ]; then
    cat > .env.local <<'EOF'
# Machine-local ArenaX development defaults. This file is git-ignored.
ARENA_POSTGRES_PASSWORD=arenax_local_only
ARENA_REDIS_PASSWORD=
ARENA_MINIO_ROOT_USER=arenax-local
ARENA_MINIO_ROOT_PASSWORD=arenax_local_only
ARENA_RABBITMQ_USER=arenax-local
ARENA_RABBITMQ_PASSWORD=arenax_local_only
ARENA_JWT_SECRET=arenax_local_jwt_secret_change_for_real_use
EOF
  fi
}

dependencies="postgres pgbouncer redis minio rabbitmq zookeeper kafka"

case "${1:-up}" in
  up)
    ensure_env
    docker compose --env-file .env.local up -d $dependencies
    printf 'Waiting for service health checks...\n'
    docker compose --env-file .env.local ps
    ;;
  status)
    ensure_env
    docker compose --env-file .env.local ps
    ;;
  seed)
    ensure_env
    docker compose --env-file .env.local exec -T postgres sh -c 'pg_isready -U "$POSTGRES_USER" -d "$POSTGRES_DB"' >/dev/null
    if [ -f .arenax-local.seeded ]; then
      printf 'Synthetic seed is already marked complete; use `scripts/local-dev.sh reset-seed` to replace it.\n'
      exit 0
    fi
    command -v npm >/dev/null 2>&1 || fail 'npm is required to seed the TypeScript service'
    if [ ! -d server/node_modules ]; then
      fail 'Server dependencies are missing; run `cd server && npm ci` before seeding'
    fi
    (cd server && DATABASE_URL='postgresql://arenax:arenax_local_only@localhost:5433/arenax?schema=public' npm run prisma:migrate:deploy >/dev/null && DATABASE_URL='postgresql://arenax:arenax_local_only@localhost:5433/arenax?schema=public' npm run prisma:seed)
    : > .arenax-local.seeded
    ;;
  reset-seed)
    rm -f .arenax-local.seeded
    printf 'Seed marker cleared; run `scripts/local-dev.sh seed` to seed again.\n'
    ;;
  down)
    ensure_env
    docker compose --env-file .env.local down
    printf 'Stopped local services. Named volumes were preserved.\n'
    ;;
  *)
    fail "Unknown command: ${1:-up}"
    ;;
esac
