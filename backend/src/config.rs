use serde::Deserialize;
use std::collections::HashMap;
use std::env;

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub database: DatabaseConfig,
    pub redis: RedisConfig,
    pub storage: StorageConfig,
    pub payments: PaymentsConfig,
    pub auth: AuthConfig,
    pub stellar: StellarConfig,
    pub ai: AiConfig,
    pub server: ServerConfig,
    pub rate_limit: RateLimitConfig,
    pub idempotency: IdempotencyConfig,
    pub notifications: NotificationsConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct DatabaseConfig {
    pub url: String,
    pub migration_mode: MigrationMode,
    /// Connection pool ceiling.
    pub max_connections: u32,
    /// How long a caller waits for a connection before the request is failed.
    /// Bounded so a saturated or unreachable database sheds load instead of
    /// letting every handler pile up on the pool (Issue #861).
    pub acquire_timeout_secs: u64,
    /// Interval between background pool health probes.
    pub health_check_interval_secs: u64,
    /// Consecutive failures that trip the circuit breaker open.
    pub circuit_failure_threshold: u32,
    /// How long the breaker stays open before admitting a trial request.
    pub circuit_open_secs: u64,
    /// PostgreSQL `statement_timeout` applied to every connection (#1084):
    /// a slow query or deadlock is cancelled instead of holding a pool
    /// connection indefinitely. `DATABASE_STATEMENT_TIMEOUT_MS`, default 5000.
    pub statement_timeout_ms: u64,
    /// Queries slower than this are logged at WARN with their SQL and
    /// duration (#1084). `DATABASE_SLOW_QUERY_THRESHOLD_MS`, default 200.
    pub slow_query_threshold_ms: u64,
}

/// Read a `u64` from the environment, falling back when unset or unparseable.
fn env_required(name: &str) -> Result<String, anyhow::Error> {
    env::var(name).map_err(|_| anyhow::anyhow!("{name} is required; set it in the service environment"))
}

/// Read an optional integer, using the default only when the variable is absent.
fn env_u64(key: &str, default: u64) -> Result<u64, anyhow::Error> {
    match env::var(key) {
        Ok(value) => value
            .trim()
            .parse()
            .map_err(|_| anyhow::anyhow!("{key} must be a non-negative integer")),
        Err(env::VarError::NotPresent) => Ok(default),
        Err(_) => anyhow::bail!("{key} must contain valid text"),
    }
}

/// Read an optional integer, using the default only when the variable is absent.
fn env_u32(key: &str, default: u32) -> Result<u32, anyhow::Error> {
    match env::var(key) {
        Ok(value) => value
            .trim()
            .parse()
            .map_err(|_| anyhow::anyhow!("{key} must be a non-negative integer")),
        Err(env::VarError::NotPresent) => Ok(default),
        Err(_) => anyhow::bail!("{key} must contain valid text"),
    }
}

// ── Secret hardening (Issue #1161) ───────────────────────────────────────────

/// Minimum number of characters accepted for a secret-bearing variable.
///
/// 32 chars is the practical floor for a high-entropy value (256 bits of
/// base64/hex output), and matches the guidance in the issue.
pub const MIN_SECRET_LENGTH: usize = 32;

/// Placeholder / legacy values that are public knowledge and therefore must
/// never be accepted, even if they happen to be long enough.
const FORBIDDEN_SECRET_VALUES: &[&str] = &[
    "default_secret_change_in_production",
    "supersecretkey",
    "changeme",
    "change_me",
    "password",
    "secret",
    "test",
];

/// Reject a secret that is empty, a known placeholder, or too short to carry
/// meaningful entropy.
///
/// Returns a message safe to surface in a startup log: it names the variable
/// and the reason, but never echoes the secret's value.
pub fn require_strong_secret(name: &str, value: &str) -> Result<(), anyhow::Error> {
    let trimmed = value.trim();

    if trimmed.is_empty() {
        anyhow::bail!("{name} is empty; refusing to start with an empty secret");
    }

    if FORBIDDEN_SECRET_VALUES
        .iter()
        .any(|weak| trimmed.eq_ignore_ascii_case(weak))
    {
        anyhow::bail!(
            "{name} is set to a well-known default value; refusing to start. \
             Generate a unique secret (e.g. `openssl rand -base64 48`) and set it in the environment"
        );
    }

    if trimmed.len() < MIN_SECRET_LENGTH {
        anyhow::bail!(
            "{name} must be at least {MIN_SECRET_LENGTH} characters long \
             (got {}); generate a stronger secret (e.g. `openssl rand -base64 48`)",
            trimmed.len()
        );
    }

    Ok(())
}

/// Read a required secret from the environment and enforce [`MIN_SECRET_LENGTH`]
/// plus the placeholder deny-list. A missing variable is reported by name.
fn env_required_secret(name: &str) -> Result<String, anyhow::Error> {
    let value = env::var(name).ok();
    validate_required_secret(name, value.as_deref())?;
    let value = value.ok_or_else(|| anyhow::anyhow!("{name} environment variable is not set"))?;
    Ok(value)
}

fn env_optional_secret(name: &str) -> Result<Option<String>, anyhow::Error> {
    let value = env::var(name).ok().filter(|value| !value.trim().is_empty());
    validate_optional_secret(name, value.as_deref())?;
    Ok(value)
}

fn validate_required_secret(name: &str, value: Option<&str>) -> Result<(), anyhow::Error> {
    let value = value.ok_or_else(|| anyhow::anyhow!("{name} environment variable is not set"))?;
    require_strong_secret(name, value)
}

/// Validate a secret that is optional to configure: when present it must meet
/// the same bar as a required one, but absence is allowed.
fn validate_optional_secret(name: &str, value: Option<&str>) -> Result<(), anyhow::Error> {
    if let Some(v) = value {
        require_strong_secret(name, v)?;
    }
    Ok(())
}

fn validate_url(name: &str, value: &str, allowed_schemes: &[&str]) -> Result<(), anyhow::Error> {
    let parsed = reqwest::Url::parse(value)
        .map_err(|_| anyhow::anyhow!("{name} must be a valid URL"))?;
    if !allowed_schemes.contains(&parsed.scheme()) || parsed.host_str().is_none() {
        anyhow::bail!("{name} must use one of these URL schemes: {}", allowed_schemes.join(", "));
    }
    Ok(())
}

fn require_https_in_production(name: &str, value: &str, production: bool) -> Result<(), anyhow::Error> {
    let parsed = reqwest::Url::parse(value)
        .map_err(|_| anyhow::anyhow!("{name} must be a valid URL"))?;
    if production && parsed.scheme() != "https" {
        anyhow::bail!("{name} must use HTTPS when APP_ENV is production");
    }
    Ok(())
}

fn validate_app_environment(value: &str) -> Result<bool, anyhow::Error> {
    match value.trim().to_ascii_lowercase().as_str() {
        "development" | "preview" => Ok(false),
        "production" => Ok(true),
        _ => anyhow::bail!("APP_ENV must be development, preview, or production"),
    }
}

fn reject_test_credential(name: &str, value: &str, production: bool, test_prefix: &str) -> Result<(), anyhow::Error> {
    if production && value.to_ascii_lowercase().starts_with(&test_prefix.to_ascii_lowercase()) {
        anyhow::bail!("{name} contains test-environment credentials; configure a production credential");
    }
    Ok(())
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
pub enum MigrationMode {
    Run,
    Disabled,
}

impl MigrationMode {
    fn from_env_value(value: &str) -> Result<Self, anyhow::Error> {
        match value.trim().to_ascii_lowercase().as_str() {
            "run" | "auto" | "true" | "1" => Ok(Self::Run),
            "disabled" | "disable" | "off" | "false" | "0" => Ok(Self::Disabled),
            other => anyhow::bail!(
                "invalid BACKEND_MIGRATION_MODE value `{}`; expected `run` or `disabled`",
                other
            ),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct RedisConfig {
    pub url: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct StorageConfig {
    pub s3_endpoint: String,
    pub s3_access_key: String,
    pub s3_secret_key: String,
    pub s3_bucket: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct PaymentsConfig {
    pub paystack_secret: Option<String>,
    pub flutterwave_secret: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AuthConfig {
    pub jwt_secret: String,
    /// Optional dedicated secret used to sign refresh tokens
    /// (`JWT_REFRESH_SECRET`). When unset, refresh tokens are signed with
    /// `jwt_secret`. When set it must satisfy the same strength rules.
    pub jwt_refresh_secret: Option<String>,
    /// Duration string for the short-lived access token, e.g. "15m".
    pub jwt_expires_in: String,
    /// Duration string for the long-lived refresh token, e.g. "7d".
    /// Defaults to "7d" when the env var is absent.
    pub jwt_refresh_expires_in: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct StellarConfig {
    pub network_url: String,
    pub admin_secret: String,
    pub soroban_contract_prize: String,
    pub soroban_contract_reputation: String,
    pub soroban_contract_arenax_token: String,
    /// Contract address for the match-lifecycle Soroban contract.
    /// Reads `SOROBAN_CONTRACT_MATCH` from the environment; falls back to
    /// `soroban_contract_prize` so existing deployments keep working without
    /// adding the new variable.
    pub soroban_contract_match: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AiConfig {
    pub model_path: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ServerConfig {
    pub port: u16,
    pub host: String,
    pub rust_log: String,
}

fn default_rate_limit_headers() -> bool {
    true
}

#[derive(Debug, Deserialize, Clone)]
pub struct RateLimitConfig {
    pub requests: u32,
    pub window: u64,
    /// Whether to send rate limit headers (`RateLimit-Limit`, `RateLimit-Remaining`,
    /// `RateLimit-Reset`) on responses. Defaults to `true`.
    #[serde(default = "default_rate_limit_headers")]
    pub headers: bool,
    /// Per-endpoint overrides for the built-in rate-limit buckets, keyed by
    /// bucket name (e.g. `auth_strict`, `tournament`, `default`). Parsed from
    /// `RATE_LIMIT_OVERRIDES` as comma-separated `bucket=limit:window` pairs.
    #[serde(default)]
    pub overrides: HashMap<String, EndpointLimitOverride>,
}

/// A single per-endpoint rate-limit override: the request ceiling and the
/// window length (in seconds) it applies to.
#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
pub struct EndpointLimitOverride {
    pub limit: u32,
    pub window_secs: u64,
}

/// Parse `RATE_LIMIT_OVERRIDES` — comma-separated `bucket=limit:window` pairs,
/// e.g. `auth_strict=3:60,tournament=5:120,default=200:60`.
///
/// Malformed entries are skipped (with a warning) instead of failing startup so
/// a single typo cannot take the API down; the built-in bucket limits remain in
/// effect for anything that does not parse.
fn parse_rate_limit_overrides(raw: &str) -> HashMap<String, EndpointLimitOverride> {
    let mut overrides = HashMap::new();
    for entry in raw.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        let Some((name, spec)) = entry.split_once('=') else {
            tracing::warn!(entry, "RATE_LIMIT_OVERRIDES: expected `bucket=limit:window`, ignoring entry");
            continue;
        };
        let Some((limit, window)) = spec.split_once(':') else {
            tracing::warn!(entry, "RATE_LIMIT_OVERRIDES: expected `limit:window`, ignoring entry");
            continue;
        };
        match (limit.trim().parse::<u32>(), window.trim().parse::<u64>()) {
            (Ok(limit), Ok(window_secs)) if limit > 0 && window_secs > 0 => {
                overrides.insert(
                    name.trim().to_ascii_lowercase(),
                    EndpointLimitOverride { limit, window_secs },
                );
            }
            _ => {
                tracing::warn!(
                    entry,
                    "RATE_LIMIT_OVERRIDES: limit/window must be positive integers, ignoring entry"
                );
            }
        }
    }
    overrides
}

#[derive(Debug, Deserialize, Clone)]
pub struct IdempotencyConfig {
    pub ttl_seconds: u64,
    pub max_response_size_kb: u32,
}

#[derive(Debug, Deserialize, Clone)]
pub struct NotificationsConfig {
    /// SendGrid API key for the email fan-out channel (#1107). Empty when
    /// unset — the email channel then fails fast rather than silently
    /// no-op'ing, so a misconfigured deployment is visible in the delivery
    /// audit log instead of just losing emails quietly.
    pub sendgrid_api_key: String,
    pub sendgrid_from_email: String,
}

impl Config {
    pub fn from_env() -> Result<Self, anyhow::Error> {
        dotenvy::dotenv().ok();

        let app_env = env::var("APP_ENV").unwrap_or_else(|_| "development".to_string());
        let production = validate_app_environment(&app_env)?;
        let database_url = env_required("DATABASE_URL")?;
        validate_url("DATABASE_URL", &database_url, &["postgres", "postgresql"])?;
        let migration_mode = env::var("BACKEND_MIGRATION_MODE")
            .map(|value| MigrationMode::from_env_value(&value))
            .unwrap_or(Ok(MigrationMode::Run))?;
        let redis_url = env_required("REDIS_URL")?;
        validate_url("REDIS_URL", &redis_url, &["redis", "rediss"])?;
        let s3_endpoint = env_required("S3_ENDPOINT")?;
        validate_url("S3_ENDPOINT", &s3_endpoint, &["http", "https"])?;
        require_https_in_production("S3_ENDPOINT", &s3_endpoint, production)?;
        let s3_access_key = env_required("S3_ACCESS_KEY")?;
        // Secret-bearing variables are validated for length / placeholder values
        // so a deployment can never boot on a publicly-known default (#1161).
        let s3_secret_key = env_required_secret("S3_SECRET_KEY")?;
        let s3_bucket = env_required("S3_BUCKET")?;
        let payment_provider = env::var("PAYMENT_PROVIDER").unwrap_or_else(|_| "paystack".into());
        let (paystack_secret, flutterwave_secret) =
            match payment_provider.trim().to_ascii_lowercase().as_str() {
                "paystack" => (
                    Some(env_required_secret("PAYSTACK_SECRET")?),
                    env_optional_secret("FLUTTERWAVE_SECRET")?,
                ),
                "flutterwave" => (
                    env_optional_secret("PAYSTACK_SECRET")?,
                    Some(env_required_secret("FLUTTERWAVE_SECRET")?),
                ),
                _ => anyhow::bail!("PAYMENT_PROVIDER must be `paystack` or `flutterwave`"),
            };
        if let Some(secret) = paystack_secret.as_deref() {
            reject_test_credential("PAYSTACK_SECRET", secret, production, "sk_test_")?;
        }
        if let Some(secret) = flutterwave_secret.as_deref() {
            reject_test_credential(
                "FLUTTERWAVE_SECRET",
                secret,
                production,
                "FLWSECK_TEST-",
            )?;
        }
        let jwt_secret = env_required_secret("JWT_SECRET")?;
        // An empty value (as shipped in env.example) means "not configured",
        // not "configured to the empty string".
        let jwt_refresh_secret = env::var("JWT_REFRESH_SECRET")
            .ok()
            .filter(|v| !v.trim().is_empty());
        validate_optional_secret("JWT_REFRESH_SECRET", jwt_refresh_secret.as_deref())?;
        let jwt_expires_in = env_required("JWT_EXPIRES_IN")?;
        let jwt_refresh_expires_in =
            env::var("JWT_REFRESH_EXPIRES_IN").unwrap_or_else(|_| "7d".to_string());
        let stellar_network_url = env_required("STELLAR_NETWORK_URL")?;
        validate_url("STELLAR_NETWORK_URL", &stellar_network_url, &["http", "https"])?;
        require_https_in_production("STELLAR_NETWORK_URL", &stellar_network_url, production)?;
        if production && stellar_network_url.to_ascii_lowercase().contains("testnet") {
            anyhow::bail!("STELLAR_NETWORK_URL points to testnet while APP_ENV is production");
        }
        let stellar_admin_secret = env_required_secret("STELLAR_ADMIN_SECRET")?;
        let soroban_contract_prize = env_required("SOROBAN_CONTRACT_PRIZE")?;
        let soroban_contract_reputation = env_required("SOROBAN_CONTRACT_REPUTATION")?;
        let soroban_contract_arenax_token = env_required("SOROBAN_CONTRACT_ARENAX_TOKEN")?;
        // Falls back to the prize contract so existing deployments don't break.
        let soroban_contract_match = env::var("SOROBAN_CONTRACT_MATCH")
            .unwrap_or_else(|_| soroban_contract_prize.clone());
        let ai_model_path = env_required("AI_MODEL_PATH")?;
        let port: u16 = env_required("PORT")?
            .parse()
            .map_err(|_| anyhow::anyhow!("PORT must be a valid port number"))?;
        let host = env_required("HOST")?;
        let rust_log = env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string());
        let rate_limit_requests: u32 = env_required("RATE_LIMIT_REQUESTS")?
            .parse()
            .map_err(|_| anyhow::anyhow!("RATE_LIMIT_REQUESTS must be a positive integer"))?;
        let rate_limit_window: u64 = env_required("RATE_LIMIT_WINDOW")?
            .parse()
            .map_err(|_| anyhow::anyhow!("RATE_LIMIT_WINDOW must be a positive integer"))?;
        let rate_limit_headers = env::var("RATE_LIMIT_HEADERS")
            .map(|v| v.trim().eq_ignore_ascii_case("true") || v.trim() == "1")
            .unwrap_or(true);
        let rate_limit_overrides =
            parse_rate_limit_overrides(&env::var("RATE_LIMIT_OVERRIDES").unwrap_or_default());
        let idempotency_ttl_seconds: u64 = env::var("IDEMPOTENCY_TTL_SECONDS")
            .unwrap_or_else(|_| "86400".to_string())
            .parse()?;
        let idempotency_max_response_size_kb: u32 = env::var("IDEMPOTENCY_MAX_RESPONSE_SIZE_KB")
            .unwrap_or_else(|_| "1024".to_string())
            .parse()?;
        let sendgrid_api_key = env::var("SENDGRID_API_KEY").unwrap_or_default();
        let sendgrid_from_email =
            env::var("SENDGRID_FROM_EMAIL").unwrap_or_else(|_| "no-reply@arenax.gg".to_string());

        Ok(Config {
            database: DatabaseConfig {
                url: database_url,
                migration_mode,
                max_connections: env_u32("DATABASE_MAX_CONNECTIONS", 20)?,
                acquire_timeout_secs: env_u64("DATABASE_ACQUIRE_TIMEOUT_SECS", 2)?,
                health_check_interval_secs: env_u64("DATABASE_HEALTH_INTERVAL_SECS", 10)?,
                circuit_failure_threshold: env_u32("DATABASE_CIRCUIT_FAILURES", 3)?,
                circuit_open_secs: env_u64("DATABASE_CIRCUIT_OPEN_SECS", 30)?,
                statement_timeout_ms: env_u64("DATABASE_STATEMENT_TIMEOUT_MS", 5000)?,
                slow_query_threshold_ms: env_u64("DATABASE_SLOW_QUERY_THRESHOLD_MS", 200)?,
            },
            redis: RedisConfig { url: redis_url },
            storage: StorageConfig {
                s3_endpoint,
                s3_access_key,
                s3_secret_key,
                s3_bucket,
            },
            payments: PaymentsConfig {
                paystack_secret,
                flutterwave_secret,
            },
            auth: AuthConfig {
                jwt_secret,
                jwt_refresh_secret,
                jwt_expires_in,
                jwt_refresh_expires_in,
            },
            stellar: StellarConfig {
                network_url: stellar_network_url,
                admin_secret: stellar_admin_secret,
                soroban_contract_prize,
                soroban_contract_reputation,
                soroban_contract_arenax_token,
                soroban_contract_match,
            },
            ai: AiConfig {
                model_path: ai_model_path,
            },
            server: ServerConfig {
                port,
                host,
                rust_log,
            },
            rate_limit: RateLimitConfig {
                requests: rate_limit_requests,
                window: rate_limit_window,
                headers: rate_limit_headers,
                overrides: rate_limit_overrides,
            },
            idempotency: IdempotencyConfig {
                ttl_seconds: idempotency_ttl_seconds,
                max_response_size_kb: idempotency_max_response_size_kb,
            },
            notifications: NotificationsConfig {
                sendgrid_api_key,
                sendgrid_from_email,
            },
        })
    }
}

#[cfg(test)]
mod secret_validation_tests {
    use super::*;

    /// A value comfortably above the minimum entropy floor.
    fn strong_secret() -> String {
        "z".repeat(MIN_SECRET_LENGTH)
    }

    #[test]
    fn accepts_secret_at_minimum_length() {
        assert!(require_strong_secret("JWT_SECRET", &strong_secret()).is_ok());
    }

    #[test]
    fn accepts_secret_longer_than_minimum() {
        let long = format!("{}{}", strong_secret(), "abcdefgh");
        assert!(require_strong_secret("JWT_SECRET", &long).is_ok());
    }

    #[test]
    fn rejects_empty_and_whitespace_secret() {
        assert!(require_strong_secret("JWT_SECRET", "").is_err());
        assert!(require_strong_secret("JWT_SECRET", "   ").is_err());
    }

    #[test]
    fn rejects_secret_below_minimum_length() {
        let short = "a".repeat(MIN_SECRET_LENGTH - 1);
        assert!(require_strong_secret("JWT_SECRET", &short).is_err());
    }

    #[test]
    fn rejects_the_legacy_hardcoded_default() {
        // The exact value `JwtConfig::default()` used to fall back to.
        assert!(
            require_strong_secret("JWT_SECRET", "default_secret_change_in_production").is_err()
        );
    }

    #[test]
    fn rejects_common_placeholders_case_insensitively() {
        assert!(require_strong_secret("JWT_SECRET", "ChangeMe").is_err());
        assert!(require_strong_secret("S3_SECRET_KEY", "SECRET").is_err());
        assert!(require_strong_secret("JWT_SECRET", "password").is_err());
    }

    #[test]
    fn error_message_names_the_variable_but_not_the_value() {
        let err = require_strong_secret("JWT_REFRESH_SECRET", "short")
            .expect_err("short secrets must be rejected");
        let msg = err.to_string();
        assert!(msg.contains("JWT_REFRESH_SECRET"), "message was: {msg}");
        assert!(!msg.contains("short"), "message leaked the secret: {msg}");
    }

    #[test]
    fn optional_secret_validation_allows_absence() {
        assert!(validate_optional_secret("JWT_REFRESH_SECRET", None).is_ok());
        assert!(
            validate_optional_secret("JWT_REFRESH_SECRET", Some(&strong_secret())).is_ok()
        );
        assert!(validate_optional_secret("JWT_REFRESH_SECRET", Some("weak")).is_err());
    }

    #[test]
    fn required_secret_validation_covers_missing_malformed_and_valid_values() {
        let strong = strong_secret();
        let cases = [
            ("missing", None, false),
            ("empty", Some("  "), false),
            ("placeholder", Some("supersecretkey"), false),
            ("too short", Some("short"), false),
            ("valid", Some(strong.as_str()), true),
        ];

        for (case, value, expected) in cases {
            assert_eq!(
                validate_required_secret("JWT_SECRET", value).is_ok(),
                expected,
                "unexpected validation result for {case}"
            );
        }
    }

    #[test]
    fn endpoint_validation_is_scheme_aware() {
        let cases = [
            ("postgres://user:pass@localhost/db", &["postgres", "postgresql"][..], true),
            ("redis://localhost:6379", &["redis", "rediss"][..], true),
            ("https://horizon.stellar.org", &["http", "https"][..], true),
            ("https://example.com", &["postgres", "postgresql"][..], false),
            ("not a URL", &["http", "https"][..], false),
        ];

        for (value, schemes, expected) in cases {
            assert_eq!(
                validate_url("ENDPOINT", value, schemes).is_ok(),
                expected,
                "unexpected validation result for {value}"
            );
        }
    }

    #[test]
    fn production_rejects_test_credentials_but_nonproduction_allows_them() {
        let cases = [
            ("sk_test_key", true, false),
            ("sk_live_key", true, true),
            ("sk_test_key", false, true),
        ];

        for (value, production, expected) in cases {
            assert_eq!(
                reject_test_credential("PAYSTACK_SECRET", value, production, "sk_test_")
                    .is_ok(),
                expected,
                "unexpected validation result for production={production}, value={value}"
            );
        }
    }

    #[test]
    fn production_urls_require_https() {
        let cases = [
            ("https://storage.example.com", true, true),
            ("http://localhost:9000", false, true),
            ("http://storage.example.com", true, false),
        ];

        for (value, production, expected) in cases {
            assert_eq!(
                require_https_in_production("S3_ENDPOINT", value, production).is_ok(),
                expected,
                "unexpected validation result for production={production}, value={value}"
            );
        }
    }
}
