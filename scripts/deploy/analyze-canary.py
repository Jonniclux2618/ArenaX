#!/usr/bin/env python3
"""Fail closed when the canary error-rate or latency telemetry is missing or bad."""

import math
import re
import sys

SAMPLE = re.compile(
    r'^([a-zA-Z_:][a-zA-Z0-9_:]*)(?:\{(.*)\})?\s+'
    r'([+-]?(?:NaN|Inf|(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)(?:[eE][+-]?[0-9]+)?))'
    r'(?:\s+.*)?$'
)
LABEL = re.compile(r'([a-zA-Z_][a-zA-Z0-9_]*)="((?:\\.|[^"])*)"')


def metrics(path):
    samples = {}
    with open(path, encoding="utf-8") as exposition:
        for line in exposition:
            match = SAMPLE.match(line.strip())
            if not match:
                continue
            name, raw_labels, raw_value = match.groups()
            labels = dict(LABEL.findall(raw_labels or ""))
            value = float(raw_value)
            if not math.isfinite(value):
                raise ValueError(f"non-finite metric sample for {name}")
            samples[(name, tuple(sorted(labels.items())))] = value
    return samples


def main():
    if len(sys.argv) != 5:
        raise ValueError("usage: analyze-canary.py BEFORE AFTER MAX_ERROR_RATE MAX_P95_SECONDS")

    before = metrics(sys.argv[1])
    after = metrics(sys.argv[2])
    max_error_rate = float(sys.argv[3])
    max_p95 = float(sys.argv[4])
    if not 0 <= max_error_rate < 1 or not math.isfinite(max_p95) or max_p95 <= 0:
        raise ValueError("canary thresholds must be finite (error rate in [0,1), p95 > 0)")

    total_requests = 0.0
    failed_requests = 0.0
    for key, current in after.items():
        if key[0] != "http_requests_total":
            continue
        previous = before.get(key, 0.0)
        delta = current - previous
        if delta < 0:
            raise ValueError("HTTP request counter reset during canary observation")
        total_requests += delta
        labels = dict(key[1])
        if labels.get("status_code", "").startswith("5"):
            failed_requests += delta

    if total_requests <= 0:
        raise ValueError("no HTTP request telemetry observed during the canary window")
    error_rate = failed_requests / total_requests

    bucket_deltas = {}
    for key, current in after.items():
        if key[0] != "http_request_duration_seconds_bucket":
            continue
        labels = dict(key[1])
        bound = labels.get("le")
        if bound is None:
            continue
        delta = current - before.get(key, 0.0)
        if delta < 0:
            raise ValueError("latency histogram reset during canary observation")
        bucket_deltas[bound] = bucket_deltas.get(bound, 0.0) + delta

    count = bucket_deltas.get("+Inf", 0.0)
    if count <= 0:
        raise ValueError("no latency histogram telemetry observed during the canary window")
    threshold_count = math.ceil(count * 0.95)
    p95 = next(
        (float(bound) for bound, observed in sorted(
            bucket_deltas.items(),
            key=lambda item: math.inf if item[0] == "+Inf" else float(item[0]),
        ) if observed >= threshold_count),
        math.inf,
    )

    print(
        f"canary requests={int(total_requests)} errors={int(failed_requests)} "
        f"error_rate={error_rate:.4f} p95_bucket_seconds={p95}"
    )
    if error_rate > max_error_rate:
        raise ValueError(f"error rate {error_rate:.4f} exceeds limit {max_error_rate:.4f}")
    if p95 > max_p95:
        raise ValueError(f"p95 bucket {p95} seconds exceeds limit {max_p95} seconds")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError) as error:
        print(f"canary analysis failed: {error}", file=sys.stderr)
        sys.exit(1)
