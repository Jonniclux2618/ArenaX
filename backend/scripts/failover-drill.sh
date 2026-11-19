#!/usr/bin/env bash
# Disposable PostgreSQL streaming-replication failover drill.
# This drill creates its own Docker network, primary, and standby; it never
# reads application credentials or connects to a configured database.
set -Eeuo pipefail

image="${POSTGRES_DRILL_IMAGE:-postgres:16.4}"
evidence_dir="${DRILL_EVIDENCE_DIR:-${TMPDIR:-/tmp}/arenax-failover-drill}"
max_rto_seconds="${DRILL_MAX_RTO_SECONDS:-3600}"
run_id="arenax-drill-$$"
network="$run_id"
primary="$run_id-primary"
standby="$run_id-standby"
volume="$run_id-standby-data"
replication_password="drill-${RANDOM}-${RANDOM}"
expected_rows=100
detected_at=0
recovered_at=0
last_acknowledged=0
recovered_rows=0
result="failed"

mkdir -p "$evidence_dir"

cleanup() {
  docker rm -f "$primary" "$standby" >/dev/null 2>&1 || true
  docker volume rm "$volume" >/dev/null 2>&1 || true
  docker network rm "$network" >/dev/null 2>&1 || true
  cat > "$evidence_dir/$run_id.json" <<EOF
{
  "run_id": "$run_id",
  "environment": "disposable-docker",
  "result": "$result",
  "detection_seconds": $detected_at,
  "recovery_seconds": $recovered_at,
  "acknowledged_rows_before_failure": $last_acknowledged,
  "rows_after_promotion": $recovered_rows,
  "data_loss_rows": $((last_acknowledged - recovered_rows)),
  "rto_target_seconds": $max_rto_seconds,
  "generated_at_utc": "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
}
EOF
}
trap cleanup EXIT

wait_ready() {
  local container="$1"
  local until=$((SECONDS + 120))
  until docker exec "$container" pg_isready -U postgres >/dev/null 2>&1; do
    if (( SECONDS >= until )); then
      echo "Timed out waiting for $container to become ready" >&2
      return 1
    fi
    sleep 1
  done
}

docker network create "$network" >/dev/null
docker volume create "$volume" >/dev/null
docker run -d --name "$primary" --network "$network" \
  -e POSTGRES_PASSWORD=drill-only \
  "$image" -c wal_level=replica -c max_wal_senders=5 \
  -c hot_standby=on >/dev/null
wait_ready "$primary"

docker exec "$primary" psql -U postgres -v ON_ERROR_STOP=1 \
  -c "CREATE ROLE replicator WITH REPLICATION LOGIN PASSWORD '$replication_password';" >/dev/null
docker exec "$primary" sh -ec \
  "printf 'host replication replicator 0.0.0.0/0 scram-sha-256\\n' >> \"\$PGDATA/pg_hba.conf\""
docker exec "$primary" psql -U postgres -v ON_ERROR_STOP=1 \
  -c "SELECT pg_reload_conf();" >/dev/null

docker run --rm --network "$network" --user postgres \
  -e PGPASSWORD="$replication_password" \
  -e PRIMARY_HOST="$primary" -e REPL_PASSWORD="$replication_password" \
  -v "$volume:/var/lib/postgresql/data" "$image" sh -ec \
  'pg_basebackup -h "$PRIMARY_HOST" -U replicator -D "$PGDATA" -Fp -Xs -P;
   touch "$PGDATA/standby.signal";
   printf "primary_conninfo = '\''host=%s user=replicator password=%s'\''\n" \
     "$PRIMARY_HOST" "$REPL_PASSWORD" >> "$PGDATA/postgresql.auto.conf"'

docker run -d --name "$standby" --network "$network" \
  -v "$volume:/var/lib/postgresql/data" "$image" \
  -c hot_standby=on >/dev/null
wait_ready "$standby"

docker exec "$primary" psql -U postgres -v ON_ERROR_STOP=1 -c \
  'CREATE TABLE failover_drill_writes (id BIGINT PRIMARY KEY);' >/dev/null
docker exec "$primary" psql -U postgres -v ON_ERROR_STOP=1 -c \
  "INSERT INTO failover_drill_writes SELECT generate_series(1, $expected_rows);" >/dev/null
last_acknowledged="$(docker exec "$primary" psql -U postgres -Atqc \
  'SELECT COALESCE(MAX(id), 0) FROM failover_drill_writes;')"

replication_deadline=$((SECONDS + 120))
until [[ "$(docker exec "$standby" psql -U postgres -Atqc \
  'SELECT COALESCE(MAX(id), 0) FROM failover_drill_writes;' 2>/dev/null || true)" == "$last_acknowledged" ]]; do
  if (( SECONDS >= replication_deadline )); then
    echo "Standby did not catch up to the last acknowledged write" >&2
    exit 1
  fi
  sleep 1
done

failure_started="$SECONDS"
docker stop "$primary" >/dev/null
detected_at=$((SECONDS - failure_started))
recovery_started="$SECONDS"
docker exec "$standby" pg_ctl -D /var/lib/postgresql/data promote -w >/dev/null
until docker exec "$standby" psql -U postgres -Atqc \
  'SELECT NOT pg_is_in_recovery();' 2>/dev/null | grep -qx t; do
  if (( SECONDS - recovery_started > max_rto_seconds )); then
    echo "Promoted standby did not become writable within the RTO target" >&2
    exit 1
  fi
  sleep 1
done
docker exec "$standby" psql -U postgres -v ON_ERROR_STOP=1 -c \
  'CREATE TABLE recovery_probe (id INTEGER PRIMARY KEY);' >/dev/null
recovered_at=$((SECONDS - recovery_started))
recovered_rows="$(docker exec "$standby" psql -U postgres -Atqc \
  'SELECT COALESCE(MAX(id), 0) FROM failover_drill_writes;')"

if (( recovered_at > max_rto_seconds )); then
  echo "Failover recovery exceeded the RTO target" >&2
  exit 1
fi
if (( recovered_rows < last_acknowledged )); then
  echo "Promoted standby lost acknowledged writes (RPO breach)" >&2
  exit 1
fi

result="passed"
echo "PASS: isolated failover recovered in ${recovered_at}s with no acknowledged write loss"
