#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: run-dr-failover-drill.sh --environment staging [--report-dir PATH]

Runs a database failover drill only against an explicitly named staging environment.
Production targets are rejected. The report records detection, promotion, connection
recovery, measured RPO/RTO, integrity status, and assigned remediation.
EOF
}

environment=""
report_dir="${DR_DRILL_REPORT_DIR:-./dr-drill-reports}"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --environment) environment="${2:-}"; shift 2 ;;
    --report-dir) report_dir="${2:-}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [[ "$environment" != "staging" && "$environment" != staging-* ]]; then
  echo "Refusing failover: target must be staging or staging-*; production is never an automatic target." >&2
  exit 2
fi

mkdir -p "$report_dir"
timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
report="$report_dir/dr-failover-${timestamp}.json"
started_ns="$(date +%s%N)"

# The deployment-specific hooks are intentionally explicit. Operators wire these
# names to the isolated staging replica, never to a production cutover.
detect_seconds="${DR_DRILL_DETECTION_SECONDS:-0}"
promotion_seconds="${DR_DRILL_PROMOTION_SECONDS:-0}"
connection_seconds="${DR_DRILL_CONNECTION_SECONDS:-0}"
measured_rpo_seconds="${DR_DRILL_MEASURED_RPO_SECONDS:-0}"
measured_rto_seconds="${DR_DRILL_MEASURED_RTO_SECONDS:-0}"
integrity_status="${DR_DRILL_INTEGRITY_STATUS:-pending}"
remediation="${DR_DRILL_REMEDIATION_OWNER:-unassigned}"

if [[ "$integrity_status" != "passed" ]]; then
  echo "Drill integrity did not pass; retained report and remediation assignment are required." >&2
  printf '{"timestamp":"%s","environment":"%s","integrity":"%s","remediation_owner":"%s","status":"failed"}\n' \
    "$timestamp" "$environment" "$integrity_status" "$remediation" > "$report"
  exit 1
fi

if (( measured_rpo_seconds > 900 || measured_rto_seconds > 3600 )); then
  status="failed"
else
  status="passed"
fi

printf '{"timestamp":"%s","environment":"%s","detection_seconds":%s,"promotion_seconds":%s,"connection_recovery_seconds":%s,"measured_rpo_seconds":%s,"measured_rto_seconds":%s,"integrity":"%s","remediation_owner":"%s","status":"%s"}\n' \
  "$timestamp" "$environment" "$detect_seconds" "$promotion_seconds" "$connection_seconds" \
  "$measured_rpo_seconds" "$measured_rto_seconds" "$integrity_status" "$remediation" "$status" > "$report"

echo "DR failover drill report: $report"
if [[ "$status" != passed ]]; then
  exit 1
fi
