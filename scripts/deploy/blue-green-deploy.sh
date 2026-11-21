#!/usr/bin/env bash
# Zero-downtime blue/green deploy with an optional canary phase, for #972.
#
# 1. Determine the idle slot (the color NOT currently receiving traffic,
#    tracked in deploy/state/active-slot).
# 2. Start the idle slot on the signature-verified image digests.
# 3. Health-check the idle slot before it receives any real traffic.
# 4. Canary: shift CANARY_WEIGHT% of traffic to it for CANARY_BAKE_SECONDS,
#    re-checking health throughout. Abort (and never touch the active
#    slot) if a health check fails during the bake.
# 5. Full cutover: reload nginx to send 100% of traffic to the new slot.
#    A graceful `nginx -s reload` finishes in-flight requests on the old
#    upstream and routes new ones to the new upstream — no dropped
#    requests, no maintenance window.
# 6. Keep the old slot running (warm) for ROLLBACK_WINDOW_SECONDS so
#    rollback.sh can flip back instantly, then stop it.
# 7. Record deployment metrics (start/end time, duration, outcome, slot
#    transition) to deploy/metrics/deployments.jsonl.
#
# Usage:
#   IMAGE_TAG=<git-sha> BACKEND_IMAGE_REF=<repo>@sha256:<digest> \
#     FRONTEND_IMAGE_REF=<repo>@sha256:<digest> PROVENANCE_VERIFIED=true \
#     ./scripts/deploy/blue-green-deploy.sh
#
# Env:
#   IMAGE_TAG                 source commit tag used for reporting
#   BACKEND_IMAGE_REF         immutable, verified backend image digest
#   FRONTEND_IMAGE_REF        immutable, verified frontend image digest
#   SOURCE_REVISION           source commit whose provenance was verified
#   PROVENANCE_VERIFIED       must be true before deployment is permitted
#   CANARY_WEIGHT            (default 10) percent of traffic sent to the canary phase
#   CANARY_BAKE_SECONDS       (default 60) how long to hold the canary split
#   ROLLBACK_WINDOW_SECONDS    (default 120) how long the old slot stays warm post-cutover
#   HEALTH_CHECK_RETRIES        (default 10) readiness probe attempts
#   HEALTH_CHECK_INTERVAL_SECONDS (default 3)
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."

: "${IMAGE_TAG:?IMAGE_TAG must be set to the image tag to deploy}"
: "${SOURCE_REVISION:?SOURCE_REVISION must identify the verified source commit}"
: "${VERIFICATION_RUN_ID:?VERIFICATION_RUN_ID must identify the provenance verification run}"
: "${BACKEND_IMAGE_REF:?verified backend image digest is required}"
: "${FRONTEND_IMAGE_REF:?verified frontend image digest is required}"
[ "${PROVENANCE_VERIFIED:-}" = "true" ] || {
  echo "Deployment requires verified release provenance." >&2
  exit 1
}
for image_ref in "$BACKEND_IMAGE_REF" "$FRONTEND_IMAGE_REF"; do
  [[ "$image_ref" =~ @sha256:[0-9a-f]{64}$ ]] || {
    echo "Deployment image references must be immutable sha256 digests: $image_ref" >&2
    exit 1
  }
done
canary_weight="${CANARY_WEIGHT:-10}"
canary_bake_seconds="${CANARY_BAKE_SECONDS:-60}"
rollback_window_seconds="${ROLLBACK_WINDOW_SECONDS:-120}"
health_retries="${HEALTH_CHECK_RETRIES:-10}"
health_interval="${HEALTH_CHECK_INTERVAL_SECONDS:-3}"
canary_max_error_rate="${CANARY_MAX_ERROR_RATE_PERCENT:-1}"
canary_max_p95_latency_ms="${CANARY_MAX_P95_LATENCY_MS:-500}"
critical_journey_url="${CANARY_CRITICAL_JOURNEY_URL:-http://localhost:8080/api/health/ready}"
canary_telemetry_url="${CANARY_TELEMETRY_URL:-}"
canary_telemetry_required="${CANARY_TELEMETRY_REQUIRED:-false}"

compose="docker compose -f deploy/docker-compose.deploy.yml"
state_dir="deploy/state"
active_file="$state_dir/active-slot"
mkdir -p "$state_dir" deploy/metrics

active_color="blue"
[ -f "$active_file" ] && active_color="$(cat "$active_file")"
idle_color="green"
[ "$active_color" = "green" ] && idle_color="blue"

start_ts=$(date -u +%s)
start_iso=$(date -u +%Y-%m-%dT%H:%M:%SZ)

echo "== Deploying $IMAGE_TAG =="
echo "Active slot: $active_color | Deploying to idle slot: $idle_color"

record_metric() {
  local status="$1"
  local end_ts end_iso duration
  end_ts=$(date -u +%s)
  end_iso=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  duration=$((end_ts - start_ts))
  printf '{"started_at":"%s","finished_at":"%s","duration_seconds":%d,"image_tag":"%s","source_revision":"%s","verification_run_id":"%s","backend_image":"%s","frontend_image":"%s","provenance_verified":%s,"from_slot":"%s","to_slot":"%s","status":"%s"}\n' \
    "$start_iso" "$end_iso" "$duration" "$IMAGE_TAG" "$SOURCE_REVISION" "$VERIFICATION_RUN_ID" \
    "$BACKEND_IMAGE_REF" "$FRONTEND_IMAGE_REF" "$PROVENANCE_VERIFIED" \
    "$active_color" "$idle_color" "$status" \
    >> deploy/metrics/deployments.jsonl
  echo "Deployment metrics recorded: status=$status duration=${duration}s"
}

health_check() {
  local color="$1"
  local attempt=1
  while [ "$attempt" -le "$health_retries" ]; do
    if $compose exec -T "backend-$color" curl -fsS "http://localhost:8080/api/health" >/dev/null 2>&1; then
      echo "backend-$color healthy (attempt $attempt/$health_retries)"
      return 0
    fi
    echo "backend-$color not ready yet (attempt $attempt/$health_retries)"
    sleep "$health_interval"
    attempt=$((attempt + 1))
  done
  return 1
}

canary_guardrail_check() {
  local color="$1"
  local telemetry error_rate latency

  if ! $compose exec -T "backend-$color" curl -fsS "$critical_journey_url" >/dev/null 2>&1; then
    echo "Critical journey failed on canary slot $color: $critical_journey_url" >&2
    return 1
  fi

  if [ "$canary_telemetry_required" = "true" ] && [ -z "$canary_telemetry_url" ]; then
    echo "Telemetry is missing: CANARY_TELEMETRY_URL is required for canary guardrails" >&2
    return 1
  fi

  if [ -n "$canary_telemetry_url" ]; then
    if ! telemetry="$(curl -fsS --max-time 5 "$canary_telemetry_url")"; then
      echo "Telemetry endpoint unavailable: $canary_telemetry_url" >&2
      return 1
    fi

    error_rate="$(printf '%s\n' "$telemetry" | awk '$1=="arenax_http_error_rate_percent"{print $2; exit}')"
    latency="$(printf '%s\n' "$telemetry" | awk '$1=="arenax_http_p95_latency_ms"{print $2; exit}')"
    if [ -z "$error_rate" ] || [ -z "$latency" ]; then
      echo "Telemetry is missing required error-rate or p95-latency series" >&2
      return 1
    fi

    if ! awk -v value="$error_rate" -v limit="$canary_max_error_rate" 'BEGIN { exit !(value <= limit) }'; then
      echo "Canary error rate $error_rate% exceeds $canary_max_error_rate%" >&2
      return 1
    fi
    if ! awk -v value="$latency" -v limit="$canary_max_p95_latency_ms" 'BEGIN { exit !(value <= limit) }'; then
      echo "Canary p95 latency ${latency}ms exceeds ${canary_max_p95_latency_ms}ms" >&2
      return 1
    fi
  fi

  echo "Canary guardrails passed: critical journey healthy${telemetry:+; telemetry within thresholds}"
  return 0
}

abort() {
  echo "DEPLOY FAILED: $1" >&2
  $compose stop "backend-$idle_color" "frontend-$idle_color" >/dev/null 2>&1 || true
  record_metric "failed"
  exit 1
}

BACKEND_IMAGE_REF="$BACKEND_IMAGE_REF" \
FRONTEND_IMAGE_REF="$FRONTEND_IMAGE_REF" \
IMAGE_TAG="$IMAGE_TAG" \
  $compose up -d "backend-$idle_color" "frontend-$idle_color"

printf '%s\n' "$BACKEND_IMAGE_REF" > "$state_dir/$idle_color-backend-image"
printf '%s\n' "$FRONTEND_IMAGE_REF" > "$state_dir/$idle_color-frontend-image"
printf '%s\n' "$SOURCE_REVISION" > "$state_dir/$idle_color-source-revision"
printf '%s\n' "$VERIFICATION_RUN_ID" > "$state_dir/$idle_color-verification-run-id"
printf '%s\n' "$PROVENANCE_VERIFIED" > "$state_dir/$idle_color-provenance-verified"

echo "-- Health-checking idle slot ($idle_color) before it takes any traffic --"
health_check "$idle_color" || abort "idle slot $idle_color failed readiness checks"

echo "-- Canary: routing ${canary_weight}% of traffic to $idle_color for ${canary_bake_seconds}s --"
./scripts/deploy/render-upstream.sh "$active_color" "$idle_color" "$canary_weight"

bake_elapsed=0
while [ "$bake_elapsed" -lt "$canary_bake_seconds" ]; do
  sleep "$health_interval"
  bake_elapsed=$((bake_elapsed + health_interval))
  if ! canary_guardrail_check "$idle_color"; then
    ./scripts/deploy/render-upstream.sh "$active_color"
    abort "$idle_color failed canary guardrails during the bake — reverted to 100% $active_color"
  fi
done

echo "-- Canary bake passed. Cutting over to 100% $idle_color --"
./scripts/deploy/render-upstream.sh "$idle_color"
echo "$idle_color" > "$active_file"
echo "$active_color" > "$state_dir/previous-slot"

echo "-- Keeping $active_color warm for ${rollback_window_seconds}s in case a fast rollback is needed --"
sleep "$rollback_window_seconds"

echo "-- Decommissioning old slot ($active_color) --"
$compose stop "backend-$active_color" "frontend-$active_color"

record_metric "success"
echo "== Deploy of $IMAGE_TAG complete. Active slot: $idle_color =="
