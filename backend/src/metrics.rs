//! Prometheus metrics export.
//!
//! Exposes a `/metrics` endpoint (Prometheus text exposition format) so a
//! Prometheus server can scrape this service the same way it already
//! scrapes `arenax-server` (see `server/infra/monitoring/prometheus.yml`),
//! giving both services a single, unified monitoring stack instead of one
//! per service.
use actix_web::{HttpResponse, Result};
use once_cell::sync::Lazy;
use prometheus::{
    Encoder, HistogramVec, IntCounterVec, IntGauge, IntGaugeVec, Opts, Registry, TextEncoder,
};
use std::sync::atomic::{AtomicU64, Ordering};

pub static REGISTRY: Lazy<Registry> = Lazy::new(Registry::new);

pub static HTTP_REQUESTS_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    let counter = IntCounterVec::new(
        Opts::new("http_requests_total", "Total HTTP requests processed"),
        &["method", "route", "status_code"],
    )
    .expect("metric can be created");
    REGISTRY
        .register(Box::new(counter.clone()))
        .expect("metric can be registered");
    counter
});

pub static HTTP_REQUEST_DURATION_SECONDS: Lazy<HistogramVec> = Lazy::new(|| {
    let histogram = HistogramVec::new(
        prometheus::HistogramOpts::new(
            "http_request_duration_seconds",
            "HTTP request latency in seconds",
        )
        .buckets(vec![
            0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
        ]),
        &["method", "route"],
    )
    .expect("metric can be created");
    REGISTRY
        .register(Box::new(histogram.clone()))
        .expect("metric can be registered");
    histogram
});

pub static DB_POOL_CONNECTIONS_ACTIVE: Lazy<IntGauge> = Lazy::new(|| {
    let gauge = IntGauge::new(
        "db_pool_connections_active",
        "Active PostgreSQL connections held by the pool",
    )
    .expect("metric can be created");
    REGISTRY
        .register(Box::new(gauge.clone()))
        .expect("metric can be registered");
    gauge
});

pub static DB_POOL_CONNECTIONS_IDLE: Lazy<IntGauge> = Lazy::new(|| {
    let gauge = IntGauge::new(
        "db_pool_connections_idle",
        "Idle PostgreSQL connections held by the pool",
    )
    .expect("metric can be created");
    REGISTRY
        .register(Box::new(gauge.clone()))
        .expect("metric can be registered");
    gauge
});

// Circuit Breaker Metrics (Issue #944)
pub static CIRCUIT_BREAKER_STATE: Lazy<IntGaugeVec> = Lazy::new(|| {
    let gauge = IntGaugeVec::new(
        Opts::new(
            "circuit_breaker_state",
            "Current state of external service circuit breaker (0=Closed, 1=HalfOpen, 2=Open)",
        ),
        &["service"],
    )
    .expect("metric can be created");
    REGISTRY
        .register(Box::new(gauge.clone()))
        .expect("metric can be registered");
    gauge
});

pub static CIRCUIT_BREAKER_REQUESTS_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    let counter = IntCounterVec::new(
        Opts::new(
            "circuit_breaker_requests_total",
            "Total external service requests processed by circuit breaker",
        ),
        &["service", "status"],
    )
    .expect("metric can be created");
    REGISTRY
        .register(Box::new(counter.clone()))
        .expect("metric can be registered");
    counter
});

pub static CIRCUIT_BREAKER_TRIPS_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    let counter = IntCounterVec::new(
        Opts::new(
            "circuit_breaker_trips_total",
            "Total number of times external service circuit breaker tripped OPEN",
        ),
        &["service"],
    )
    .expect("metric can be created");
    REGISTRY
        .register(Box::new(counter.clone()))
        .expect("metric can be registered");
    counter
});

// Dependency health gauge (Issue #1079)
pub static DEPENDENCY_HEALTH: Lazy<IntGaugeVec> = Lazy::new(|| {
    let gauge = IntGaugeVec::new(
        Opts::new(
            "dependency_health",
            "Health of downstream dependencies (1=ok, 0=failed) from the readiness probe",
        ),
        &["service"],
    )
    .expect("metric can be created");
    REGISTRY
        .register(Box::new(gauge.clone()))
        .expect("metric can be registered");
    gauge
});

// Feature-journey SLO indicators (Issue #285).
pub static FEATURE_JOURNEY_REQUESTS_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    let counter = IntCounterVec::new(
        Opts::new(
            "feature_journey_requests_total",
            "Critical user journeys with synthetic and real outcome",
        ),
        &["journey", "outcome", "component"],
    )
    .expect("metric can be created");
    REGISTRY
        .register(Box::new(counter.clone()))
        .expect("metric can be registered");
    counter
});

pub static FEATURE_JOURNEY_DURATION_SECONDS: Lazy<HistogramVec> = Lazy::new(|| {
    let histogram = HistogramVec::new(
        prometheus::HistogramOpts::new(
            "feature_journey_duration_seconds",
            "End-to-end duration for critical user journeys",
        )
        .buckets(vec![0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0]),
        &["journey", "component"],
    )
    .expect("metric can be created");
    REGISTRY
        .register(Box::new(histogram.clone()))
        .expect("metric can be registered");
    histogram
});

pub static PROFILE_CACHE_REQUESTS_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    let counter = IntCounterVec::new(
        Opts::new("profile_cache_requests_total", "Profile cache reads by outcome"),
        &["outcome"],
    )
    .expect("metric can be created");
    REGISTRY
        .register(Box::new(counter.clone()))
        .expect("metric can be registered");
    counter
});

pub static CACHE_HIT_RATE: Lazy<IntGauge> = Lazy::new(|| {
    let gauge = IntGauge::new("cache_hit_rate", "Profile cache hit rate in percent")
        .expect("metric can be created");
    REGISTRY
        .register(Box::new(gauge.clone()))
        .expect("metric can be registered");
    gauge
});

static PROFILE_CACHE_HITS: AtomicU64 = AtomicU64::new(0);
static PROFILE_CACHE_READS: AtomicU64 = AtomicU64::new(0);

// ── Soroban transactions (#1070) ─────────────────────────────────────────────
pub static SOROBAN_TX_SUBMITTED_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    let counter = IntCounterVec::new(
        Opts::new(
            "soroban_tx_submitted_total",
            "Soroban contract invocations attempted",
        ),
        &["contract", "method"],
    )
    .expect("metric can be created");
    REGISTRY
        .register(Box::new(counter.clone()))
        .expect("metric can be registered");
    counter
});

pub static SOROBAN_TX_SUCCESS_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    let counter = IntCounterVec::new(
        Opts::new(
            "soroban_tx_success_total",
            "Soroban transactions confirmed successful on-chain",
        ),
        &["contract", "method"],
    )
    .expect("metric can be created");
    REGISTRY
        .register(Box::new(counter.clone()))
        .expect("metric can be registered");
    counter
});

pub static SOROBAN_TX_FAILED_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    let counter = IntCounterVec::new(
        Opts::new(
            "soroban_tx_failed_total",
            "Soroban transactions that ended without a confirmed success",
        ),
        &["contract", "method", "reason"],
    )
    .expect("metric can be created");
    REGISTRY
        .register(Box::new(counter.clone()))
        .expect("metric can be registered");
    counter
});

pub static SOROBAN_TX_RETRIES_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    let counter = IntCounterVec::new(
        Opts::new(
            "soroban_tx_retries_total",
            "Status-poll retries while waiting for a Soroban transaction to confirm",
        ),
        &["contract", "method"],
    )
    .expect("metric can be created");
    REGISTRY
        .register(Box::new(counter.clone()))
        .expect("metric can be registered");
    counter
});

pub static SOROBAN_TX_LATENCY_SECONDS: Lazy<HistogramVec> = Lazy::new(|| {
    let histogram = HistogramVec::new(
        prometheus::HistogramOpts::new(
            "soroban_tx_latency_seconds",
            "End-to-end time from invoke to confirmed success, in seconds",
        )
        .buckets(vec![0.5, 1.0, 2.5, 5.0, 10.0, 20.0, 30.0, 60.0, 120.0]),
        &["contract", "method"],
    )
    .expect("metric can be created");
    REGISTRY
        .register(Box::new(histogram.clone()))
        .expect("metric can be registered");
    histogram
});

pub static SOROBAN_DLQ_DEPTH: Lazy<IntGauge> = Lazy::new(|| {
    let gauge = IntGauge::new(
        "soroban_dlq_depth",
        "Entries currently in the Soroban transaction dead-letter queue",
    )
    .expect("metric can be created");
    REGISTRY
        .register(Box::new(gauge.clone()))
        .expect("metric can be registered");
    gauge
});

// ── Analytics ingestion (#251) ───────────────────────────────────────────────
/// Outcomes of analytics event ingestion at the canonical boundary.
/// `outcome` is one of `accepted`, `deduplicated`, or `rejected`; `reason`
/// is only meaningful for `rejected` (e.g. `malformed`, `schema_invalid`,
/// `unsupported_schema_version`, `unauthorized`) and is `none` otherwise.
pub static ANALYTICS_EVENTS_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    let counter = IntCounterVec::new(
        Opts::new(
            "analytics_events_total",
            "Analytics events processed at the ingestion boundary by outcome",
        ),
        &["outcome", "reason"],
    )
    .expect("metric can be created");
    REGISTRY
        .register(Box::new(counter.clone()))
        .expect("metric can be registered");
    counter
});

/// Analytics events that were retried (same stable event ID delivered again)
/// and converged to a single accepted event.
pub static ANALYTICS_EVENTS_DEDUPLICATED_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    let counter = IntCounterVec::new(
        Opts::new(
            "analytics_events_deduplicated_total",
            "Analytics events rejected as duplicates of an already-accepted event ID",
        ),
        &["schema_version"],
    )
    .expect("metric can be created");
    REGISTRY
        .register(Box::new(counter.clone()))
        .expect("metric can be registered");
    counter
});

/// Analytics events that entered the diagnosable rejection path.
pub static ANALYTICS_EVENTS_REJECTED_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    let counter = IntCounterVec::new(
        Opts::new(
            "analytics_events_rejected_total",
            "Analytics events rejected at ingestion, labeled by reason",
        ),
        &["reason"],
    )
    .expect("metric can be created");
    REGISTRY
        .register(Box::new(counter.clone()))
        .expect("metric can be registered");
    counter
});

/// Identity link/unlink operations recorded by the analytics boundary.
pub static ANALYTICS_IDENTITY_LINKS_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    let counter = IntCounterVec::new(
        Opts::new(
            "analytics_identity_links_total",
            "Analytics identity link/unlink operations by action and outcome",
        ),
        &["action", "outcome"],
    )
    .expect("metric can be created");
    REGISTRY
        .register(Box::new(counter.clone()))
        .expect("metric can be registered");
    counter
});

// ── Analytics dashboard query budgets (#254) ─────────────────────────────────
/// Dashboard query latency in seconds, labeled by dashboard query name.
pub static ANALYTICS_DASHBOARD_QUERY_DURATION_SECONDS: Lazy<HistogramVec> = Lazy::new(|| {
    let histogram = HistogramVec::new(
        prometheus::HistogramOpts::new(
            "analytics_dashboard_query_duration_seconds",
            "Analytics dashboard query latency in seconds, labeled by query name",
        )
        .buckets(vec![
            0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 3.0, 5.0, 10.0, 30.0,
        ]),
        &["query_name"],
    )
    .expect("metric can be created");
    REGISTRY
        .register(Box::new(histogram.clone()))
        .expect("metric can be registered");
    histogram
});

/// Dashboard query budget violations. `budget` is `latency` or `freshness`;
/// `query_name` identifies the dashboard query that exceeded its budget.
pub static ANALYTICS_DASHBOARD_BUDGET_VIOLATIONS_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    let counter = IntCounterVec::new(
        Opts::new(
            "analytics_dashboard_budget_violations_total",
            "Analytics dashboard queries that exceeded their latency or freshness budget",
        ),
        &["query_name", "budget"],
    )
    .expect("metric can be created");
    REGISTRY
        .register(Box::new(counter.clone()))
        .expect("metric can be registered");
    counter
});

/// Dashboard responses served in a degraded (stale or incomplete) state.
pub static ANALYTICS_DASHBOARD_DEGRADED_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    let counter = IntCounterVec::new(
        Opts::new(
            "analytics_dashboard_degraded_total",
            "Analytics dashboard responses served with a stale or incomplete state",
        ),
        &["query_name", "state"],
    )
    .expect("metric can be created");
    REGISTRY
        .register(Box::new(counter.clone()))
        .expect("metric can be registered");
    counter
});

/// Count of WebSocket messages dropped for exceeding the per-connection
/// rate limit (#1083), labeled by user so a single flooding user stands out.
pub static WS_MESSAGES_RATE_LIMITED_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    let counter = IntCounterVec::new(
        Opts::new(
            "ws_messages_rate_limited_total",
            "Total WebSocket messages dropped for exceeding the per-connection rate limit",
        ),
        &["user_id"],
    )
    .expect("metric can be created");
    REGISTRY
        .register(Box::new(counter.clone()))
        .expect("metric can be registered");
    counter
});

/// Per-named-query latency (#1084). `sqlx`'s own connection-level slow-query
/// log (see `db.rs::create_pool`) covers every query automatically, but it
/// has no `query_name` label to build a P99-per-query Prometheus alert from —
/// that needs the caller to name the query, via `time_query` below.
pub static DB_QUERY_DURATION_SECONDS: Lazy<HistogramVec> = Lazy::new(|| {
    let histogram = HistogramVec::new(
        prometheus::HistogramOpts::new(
            "db_query_duration_seconds",
            "Database query latency in seconds, labeled by a caller-assigned query name",
        )
        .buckets(vec![
            0.005, 0.01, 0.025, 0.05, 0.1, 0.2, 0.5, 1.0, 2.5, 5.0, 10.0,
        ]),
        &["query_name"],
    )
    .expect("metric can be created");
    REGISTRY
        .register(Box::new(histogram.clone()))
        .expect("metric can be registered");
    histogram
});

/// Same default as `DatabaseConfig::slow_query_threshold_ms` (#1084);
/// duplicated here (rather than threading `Config` into every service) so
/// `time_query` call sites don't need config access, just the pool they
/// already have.
static SLOW_QUERY_THRESHOLD_MS: Lazy<u64> = Lazy::new(|| {
    std::env::var("DATABASE_SLOW_QUERY_THRESHOLD_MS")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(200)
});

/// Runs `fut`, recording its wall-clock duration into
/// `db_query_duration_seconds{query_name}` and logging at WARN (with the
/// query name, duration, and whatever `user_id`/`correlation_id`/`route`
/// fields are on the ambient tracing span) when it exceeds
/// `DATABASE_SLOW_QUERY_THRESHOLD_MS` (#1084).
///
/// `sqlx`'s built-in slow-statement log (`db.rs`) already covers every query
/// with the raw SQL text; this is the complementary per-named-query metric
/// and log line used to build per-endpoint P99 dashboards/alerts.
pub async fn time_query<T, E>(
    query_name: &'static str,
    fut: impl std::future::Future<Output = Result<T, E>>,
) -> Result<T, E> {
    let start = std::time::Instant::now();
    let result = fut.await;
    let elapsed = start.elapsed();

    DB_QUERY_DURATION_SECONDS
        .with_label_values(&[query_name])
        .observe(elapsed.as_secs_f64());

    if elapsed.as_millis() as u64 >= *SLOW_QUERY_THRESHOLD_MS {
        tracing::warn!(
            query_name,
            duration_ms = elapsed.as_millis() as u64,
            "Slow database query"
        );
    }

    result
}

/// Force all lazily-registered metrics to initialize (and therefore
/// register with the collector registry) at startup, before the first
/// scrape — otherwise a metric with no observations yet simply wouldn't
/// appear in `/metrics` output.
pub fn init_metrics() {
    Lazy::force(&HTTP_REQUESTS_TOTAL);
    Lazy::force(&HTTP_REQUEST_DURATION_SECONDS);
    Lazy::force(&DB_POOL_CONNECTIONS_ACTIVE);
    Lazy::force(&DB_POOL_CONNECTIONS_IDLE);
    Lazy::force(&CIRCUIT_BREAKER_STATE);
    Lazy::force(&CIRCUIT_BREAKER_REQUESTS_TOTAL);
    Lazy::force(&CIRCUIT_BREAKER_TRIPS_TOTAL);
    Lazy::force(&PROFILE_CACHE_REQUESTS_TOTAL);
    Lazy::force(&CACHE_HIT_RATE);
    Lazy::force(&DEPENDENCY_HEALTH);
    Lazy::force(&SOROBAN_TX_SUBMITTED_TOTAL);
    Lazy::force(&SOROBAN_TX_SUCCESS_TOTAL);
    Lazy::force(&SOROBAN_TX_FAILED_TOTAL);
    Lazy::force(&SOROBAN_TX_RETRIES_TOTAL);
    Lazy::force(&SOROBAN_TX_LATENCY_SECONDS);
    Lazy::force(&SOROBAN_DLQ_DEPTH);
    Lazy::force(&ANALYTICS_EVENTS_TOTAL);
    Lazy::force(&ANALYTICS_EVENTS_DEDUPLICATED_TOTAL);
    Lazy::force(&ANALYTICS_EVENTS_REJECTED_TOTAL);
    Lazy::force(&ANALYTICS_IDENTITY_LINKS_TOTAL);
    Lazy::force(&ANALYTICS_DASHBOARD_QUERY_DURATION_SECONDS);
    Lazy::force(&ANALYTICS_DASHBOARD_BUDGET_VIOLATIONS_TOTAL);
    Lazy::force(&ANALYTICS_DASHBOARD_DEGRADED_TOTAL);

    // Process-level metrics (process_resident_memory_bytes, process_cpu_seconds_total,
    // open fds, ...) — only available on Linux in prometheus crate.
    #[cfg(target_os = "linux")]
    if let Err(e) = REGISTRY.register(Box::new(
        prometheus::process_collector::ProcessCollector::for_self(),
    )) {
        tracing::warn!(error = %e, "failed to register process metrics collector");
    }
}

pub fn record_profile_cache_hit() {
    PROFILE_CACHE_REQUESTS_TOTAL
        .with_label_values(&["hit"])
        .inc();
    PROFILE_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
    let reads = PROFILE_CACHE_READS.fetch_add(1, Ordering::Relaxed) + 1;
    CACHE_HIT_RATE.set((PROFILE_CACHE_HITS.load(Ordering::Relaxed) * 100 / reads) as i64);
}

pub fn record_profile_cache_miss() {
    PROFILE_CACHE_REQUESTS_TOTAL
        .with_label_values(&["miss"])
        .inc();
    let reads = PROFILE_CACHE_READS.fetch_add(1, Ordering::Relaxed) + 1;
    CACHE_HIT_RATE.set((PROFILE_CACHE_HITS.load(Ordering::Relaxed) * 100 / reads) as i64);
}

/// Snapshot the DB pool's active/idle connection counts into the gauges
/// above. Called periodically by a background task in `main.rs`.
pub fn record_pool_stats(size: u32, idle: usize) {
    DB_POOL_CONNECTIONS_ACTIVE.set(size as i64 - idle as i64);
    DB_POOL_CONNECTIONS_IDLE.set(idle as i64);
}

/// Record circuit breaker state metric snapshot for an external service.
pub fn record_circuit_breaker_state(service: &str, state_code: i64) {
    CIRCUIT_BREAKER_STATE
        .with_label_values(&[service])
        .set(state_code);
}

/// Record a circuit breaker request outcome.
pub fn record_circuit_breaker_request(service: &str, status: &str) {
    CIRCUIT_BREAKER_REQUESTS_TOTAL
        .with_label_values(&[service, status])
        .inc();
}

/// Record a circuit breaker trip event.
pub fn record_circuit_breaker_trip(service: &str) {
    CIRCUIT_BREAKER_TRIPS_TOTAL
        .with_label_values(&[service])
        .inc();
}

/// Update the `dependency_health{service}` gauge (1=ok, 0=failed) from a
/// readiness check (Issue #1079).
pub fn record_dependency_health(service: &str, healthy: bool) {
    DEPENDENCY_HEALTH
        .with_label_values(&[service])
        .set(if healthy { 1 } else { 0 });
}

/// A Soroban contract invocation was attempted.
pub fn record_soroban_submitted(contract: &str, method: &str) {
    SOROBAN_TX_SUBMITTED_TOTAL
        .with_label_values(&[contract, method])
        .inc();
}

/// A Soroban transaction was confirmed successful, `elapsed_secs` after the
/// invocation started.
pub fn record_soroban_success(contract: &str, method: &str, elapsed_secs: f64) {
    SOROBAN_TX_SUCCESS_TOTAL
        .with_label_values(&[contract, method])
        .inc();
    SOROBAN_TX_LATENCY_SECONDS
        .with_label_values(&[contract, method])
        .observe(elapsed_secs);
}

/// A Soroban invocation ended without a confirmed success. `reason` is one of
/// a fixed set of values chosen in `SorobanService::invoke`.
pub fn record_soroban_failed(contract: &str, method: &str, reason: &str) {
    SOROBAN_TX_FAILED_TOTAL
        .with_label_values(&[contract, method, reason])
        .inc();
}

/// One more status poll was needed before the transaction settled.
pub fn record_soroban_retry(contract: &str, method: &str) {
    SOROBAN_TX_RETRIES_TOTAL
        .with_label_values(&[contract, method])
        .inc();
}

/// Set the current dead-letter queue depth.
///
/// Nothing calls this yet: the backend has no Soroban DLQ table (the handler
/// module declared for it by #864 was never added). Whatever stores
/// dead-lettered transactions should call this with its row count so
/// `soroban_dlq_depth` and the `SorobanDlqBacklog` alert reflect it.
pub fn set_soroban_dlq_depth(depth: i64) {
    SOROBAN_DLQ_DEPTH.set(depth);
}

/// Record an analytics event accepted at the ingestion boundary.
pub fn record_analytics_event_accepted(schema_version: &str) {
    ANALYTICS_EVENTS_TOTAL
        .with_label_values(&["accepted", "none"])
        .inc();
    let _ = schema_version;
}

/// Record an analytics event that was a duplicate of an already-accepted
/// stable event ID (retry converged to one accepted event).
pub fn record_analytics_event_deduplicated(schema_version: &str) {
    ANALYTICS_EVENTS_TOTAL
        .with_label_values(&["deduplicated", "none"])
        .inc();
    ANALYTICS_EVENTS_DEDUPLICATED_TOTAL
        .with_label_values(&[schema_version])
        .inc();
}

/// Record an analytics event that entered the diagnosable rejection path.
pub fn record_analytics_event_rejected(reason: &str) {
    ANALYTICS_EVENTS_TOTAL
        .with_label_values(&["rejected", reason])
        .inc();
    ANALYTICS_EVENTS_REJECTED_TOTAL
        .with_label_values(&[reason])
        .inc();
}

/// Record an analytics identity link/unlink operation.
/// `action` is `link` or `unlink`; `outcome` is `accepted` or `rejected`.
pub fn record_analytics_identity_link(action: &str, outcome: &str) {
    ANALYTICS_IDENTITY_LINKS_TOTAL
        .with_label_values(&[action, outcome])
        .inc();
}

/// Record a dashboard query's latency and flag a latency budget violation
/// when it exceeds `budget_secs`.
pub fn record_analytics_dashboard_query(query_name: &str, elapsed_secs: f64, budget_secs: f64) {
    ANALYTICS_DASHBOARD_QUERY_DURATION_SECONDS
        .with_label_values(&[query_name])
        .observe(elapsed_secs);
    if elapsed_secs > budget_secs {
        ANALYTICS_DASHBOARD_BUDGET_VIOLATIONS_TOTAL
            .with_label_values(&[query_name, "latency"])
            .inc();
        tracing::warn!(
            query_name,
            elapsed_secs,
            budget_secs,
            "Analytics dashboard query exceeded its latency budget"
        );
    }
}

/// Record a dashboard query that exceeded its freshness budget (the data
/// returned is older than the documented target).
pub fn record_analytics_dashboard_stale(query_name: &str, age_secs: f64, budget_secs: f64) {
    ANALYTICS_DASHBOARD_BUDGET_VIOLATIONS_TOTAL
        .with_label_values(&[query_name, "freshness"])
        .inc();
    ANALYTICS_DASHBOARD_DEGRADED_TOTAL
        .with_label_values(&[query_name, "stale"])
        .inc();
    tracing::warn!(
        query_name,
        age_secs,
        budget_secs,
        "Analytics dashboard query exceeded its freshness budget"
    );
}

/// Record a dashboard response served in a degraded (incomplete) state,
/// e.g. because a bounded time range or pagination limit was hit.
pub fn record_analytics_dashboard_incomplete(query_name: &str) {
    ANALYTICS_DASHBOARD_DEGRADED_TOTAL
        .with_label_values(&[query_name, "incomplete"])
        .inc();
}

pub async fn metrics_handler() -> Result<HttpResponse> {
    let encoder = TextEncoder::new();
    let metric_families = REGISTRY.gather();
    let mut buffer = Vec::new();
    if let Err(e) = encoder.encode(&metric_families, &mut buffer) {
        tracing::error!(error = %e, "failed to encode prometheus metrics");
        return Ok(HttpResponse::InternalServerError().finish());
    }

    Ok(HttpResponse::Ok()
        .content_type(encoder.format_type())
        .body(buffer))
}

#[cfg(test)]
mod time_query_tests {
    use super::*;

    #[tokio::test]
    async fn records_duration_and_passes_through_ok_results() {
        let result: Result<i32, ()> =
            time_query("test.ok_query", async { Ok(42) }).await;

        assert_eq!(result, Ok(42));
        assert!(
            DB_QUERY_DURATION_SECONDS
                .with_label_values(&["test.ok_query"])
                .get_sample_count()
                >= 1
        );
    }

    #[tokio::test]
    async fn passes_through_errors_and_still_records_duration() {
        let result: Result<i32, &str> =
            time_query("test.err_query", async { Err("boom") }).await;

        assert_eq!(result, Err("boom"));
        assert!(
            DB_QUERY_DURATION_SECONDS
                .with_label_values(&["test.err_query"])
                .get_sample_count()
                >= 1
        );
    }
}

#[cfg(test)]
mod analytics_metrics_tests {
    use super::*;

    #[test]
    fn accepted_and_deduplicated_events_are_counted_separately() {
        record_analytics_event_accepted("v1");
        record_analytics_event_deduplicated("v1");

        assert!(
            ANALYTICS_EVENTS_TOTAL
                .with_label_values(&["accepted", "none"])
                .get() >= 1
        );
        assert!(
            ANALYTICS_EVENTS_TOTAL
                .with_label_values(&["deduplicated", "none"])
                .get() >= 1
        );
        assert!(
            ANALYTICS_EVENTS_DEDUPLICATED_TOTAL
                .with_label_values(&["v1"])
                .get() >= 1
        );
    }

    #[test]
    fn rejected_events_are_labeled_by_reason() {
        record_analytics_event_rejected("schema_invalid");

        assert!(
            ANALYTICS_EVENTS_REJECTED_TOTAL
                .with_label_values(&["schema_invalid"])
                .get() >= 1
        );
        assert!(
            ANALYTICS_EVENTS_TOTAL
                .with_label_values(&["rejected", "schema_invalid"])
                .get() >= 1
        );
    }

    #[test]
    fn identity_links_are_labeled_by_action_and_outcome() {
        record_analytics_identity_link("link", "accepted");
        record_analytics_identity_link("unlink", "rejected");

        assert!(
            ANALYTICS_IDENTITY_LINKS_TOTAL
                .with_label_values(&["link", "accepted"])
                .get() >= 1
        );
        assert!(
            ANALYTICS_IDENTITY_LINKS_TOTAL
                .with_label_values(&["unlink", "rejected"])
                .get() >= 1
        );
    }

    #[test]
    fn latency_budget_violation_is_recorded_only_when_exceeded() {
        record_analytics_dashboard_query("test.within_budget", 0.1, 1.0);
        assert_eq!(
            ANALYTICS_DASHBOARD_BUDGET_VIOLATIONS_TOTAL
                .with_label_values(&["test.within_budget", "latency"])
                .get(),
            0
        );

        record_analytics_dashboard_query("test.over_budget", 2.5, 1.0);
        assert!(
            ANALYTICS_DASHBOARD_BUDGET_VIOLATIONS_TOTAL
                .with_label_values(&["test.over_budget", "latency"])
                .get() >= 1
        );
    }

    #[test]
    fn stale_and_incomplete_states_are_recorded() {
        record_analytics_dashboard_stale("test.stale", 600.0, 300.0);
        record_analytics_dashboard_incomplete("test.incomplete");

        assert!(
            ANALYTICS_DASHBOARD_BUDGET_VIOLATIONS_TOTAL
                .with_label_values(&["test.stale", "freshness"])
                .get() >= 1
        );
        assert!(
            ANALYTICS_DASHBOARD_DEGRADED_TOTAL
                .with_label_values(&["test.stale", "stale"])
                .get() >= 1
        );
        assert!(
            ANALYTICS_DASHBOARD_DEGRADED_TOTAL
                .with_label_values(&["test.incomplete", "incomplete"])
                .get() >= 1
        );
    }
}
