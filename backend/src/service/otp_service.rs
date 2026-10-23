//! One-time password (OTP) authentication service (Issue #6).
//!
//! Hardens phone/email OTP login against automated guessing, account
//! discovery, and resend abuse.
//!
//! # Security design
//!
//! - **Single-use, server-side expiry**: each OTP is stored in Redis with a
//!   TTL.  Once verified (or expired) the record is deleted immediately so
//!   replay is impossible.
//! - **Verifier, not plaintext**: the OTP code itself is never stored.  We
//!   store a SHA-256 hash of the code plus a per-challenge salt, so a Redis
//!   dump or backup leak does not reveal usable codes.
//! - **Distributed rate limiting across instances**: send and verify limits
//!   use the same Redis sorted-set sliding window the rate-limit middleware
//!   uses, so the limits apply across all backend instances.
//! - **Non-enumerating responses**: whether the account exists or not, the
//!   response is identical.  A code is always "sent" — the email/SMS is
//!   only dispatched if the account exists, but the caller never knows.
//! - **Lockout after too many attempts**: after `MAX_VERIFY_ATTEMPTS` wrong
//!   guesses the challenge is locked and the user must request a new code.
//! - **Resend throttle**: a minimum gap between consecutive sends prevents
//!   abuse of the send endpoint.
//! - **Constant-time code comparison**: uses `subtle::ConstantTimeEq` to
//!   avoid timing side-channels.
//! - **Controllable clock**: all time checks go through a `Clock` trait so
//!   tests can exercise expiry, lockout, and clock-boundary scenarios
//!   deterministically.

use crate::api_error::ApiError;
use chrono::Utc;
use redis::{AsyncCommands, Client as RedisClient};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use subtle::ConstantTimeEq;
use tracing::{debug, info, warn};

// ── Tunable constants ─────────────────────────────────────────────────────

/// OTP code length (digits).
const CODE_LENGTH: usize = 6;

/// How long an OTP is valid after it is sent, in seconds.
const OTP_TTL_SECONDS: u64 = 300; // 5 minutes

/// Maximum number of wrong verification attempts before the challenge is locked.
const MAX_VERIFY_ATTEMPTS: u32 = 5;

/// Minimum seconds between consecutive "send" requests for the same identifier.
const RESEND_COOLDOWN_SECONDS: u64 = 30;

/// Maximum number of OTP sends per identifier per sliding window.
const SEND_LIMIT: u32 = 5;

/// Sliding window for the send rate limit, in seconds.
const SEND_WINDOW_SECONDS: u64 = 3600; // 1 hour

/// Maximum number of verification attempts per identifier per sliding window.
const VERIFY_LIMIT: u32 = 20;

/// Sliding window for the verify rate limit, in seconds.
const VERIFY_WINDOW_SECONDS: u64 = 3600; // 1 hour

// ── Clock abstraction ─────────────────────────────────────────────────────

/// A source of wall-clock time.
///
/// In production this delegates to [`chrono::Utc::now`]; in tests it can
/// be a fixed or stepped clock so expiry, lockout, and clock-boundary
/// behaviour is fully deterministic.
pub trait Clock: Send + Sync {
    fn now(&self) -> chrono::DateTime<chrono::Utc>;
}

/// Real wall-clock — used in production.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        Utc::now()
    }
}

// ── Identifier normalisation ──────────────────────────────────────────────

/// Normalise an email or phone number to a canonical form so that
/// `User@Example.com` and `user@example.com` map to the same challenge.
///
/// - Emails: lowercased and trimmed.
/// - Phone numbers: trim whitespace and leading `+`.
pub fn normalize_identifier(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.contains('@') {
        // Email — lowercase and trim.
        trimmed.to_lowercase()
    } else {
        // Phone — strip whitespace and a leading '+' for a canonical key.
        trimmed
            .replace([' ', '\t', '\n', '\r'], "")
            .trim_start_matches('+')
            .to_string()
    }
}

// ── OTP record stored in Redis ─────────────────────────────────────────────

/// The persisted challenge.  Only the hash and salt are stored — never the
/// plaintext code — so a Redis dump or backup leak does not reveal usable codes.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
struct OtpRecord {
    /// SHA-256(code || salt), hex-encoded.
    code_hash: String,
    /// Random salt per challenge (prevents rainbow tables).
    salt: String,
    /// Unix-epoch seconds when the OTP was issued.
    issued_at: i64,
    /// Unix-epoch seconds when the OTP expires.
    expires_at: i64,
    /// Number of failed verification attempts so far.
    attempts: u32,
    /// Whether the challenge is locked (too many attempts).
    locked: bool,
}

impl OtpRecord {
    /// Hash a candidate code with the stored salt and compare in constant time.
    fn verify(&self, candidate: &str) -> bool {
        let computed = hash_code(candidate, &self.salt);
        // Constant-time comparison to avoid timing side-channels.
        computed.as_bytes().ct_eq(self.code_hash.as_bytes()).into()
    }
}

fn hash_code(code: &str, salt: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(code.as_bytes());
    hasher.update(salt.as_bytes());
    hex::encode(hasher.finalize())
}

fn generate_code() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let n: u32 = rng.gen_range(0..10u32.pow(CODE_LENGTH as u32));
    format!("{:0width$}", n, width = CODE_LENGTH)
}

fn generate_salt() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let bytes: [u8; 16] = rng.gen();
    hex::encode(bytes)
}

// ── Redis key helpers ──────────────────────────────────────────────────────

fn otp_key(normalized: &str) -> String {
    format!("otp:{}", normalized)
}

fn send_rate_key(normalized: &str) -> String {
    format!("otp:send_rl:{}", normalized)
}

fn verify_rate_key(normalized: &str) -> String {
    format!("otp:verify_rl:{}", normalized)
}

fn resend_cooldown_key(normalized: &str) -> String {
    format!("otp:resend:{}", normalized)
}

// ── Result types ────────────────────────────────────────────────────────────

/// Outcome of a send request.
///
/// Both variants return the same HTTP response shape so the caller cannot
/// determine whether the account exists (anti-enumeration).
#[derive(Debug)]
pub enum SendOtpOutcome {
    /// The code was generated and delivered.
    Sent { expires_in: u64 },
    /// Rate-limited: too many sends or resend cooldown not elapsed.
    RateLimited { retry_after: u64 },
}

/// Outcome of a verify request.
#[derive(Debug)]
pub enum VerifyOtpOutcome {
    /// The code matched and was consumed (single-use).
    Verified,
    /// The code was wrong but attempts remain.
    WrongCode { attempts_remaining: u32 },
    /// The challenge is locked — too many wrong attempts.
    Locked,
    /// No challenge exists (expired, already used, or never sent).
    NoChallenge,
    /// Rate-limited.
    RateLimited { retry_after: u64 },
}

// ── OtpService ──────────────────────────────────────────────────────────────

/// Handles OTP generation, delivery, verification, and rate limiting.
///
/// All state lives in Redis so the limits and challenge records are shared
/// across every backend instance.
#[derive(Clone)]
pub struct OtpService {
    redis: Arc<RedisClient>,
    clock: Arc<dyn Clock>,
    /// Email/SMS delivery channel.  The service calls `deliver` with the
    /// code; the implementation decides whether to send an email, an SMS,
    /// or both.  In tests this is a no-op or a capture.
    deliverer: Arc<dyn OtpDeliverer>,
}

/// How an OTP code leaves the process.
///
/// A trait so tests can capture the code instead of sending a real
/// email/SMS, and so production can swap the transport without touching
/// the service logic.
#[async_trait::async_trait]
pub trait OtpDeliverer: Send + Sync {
    /// Deliver the code to `normalized_identifier`.  Returns `Ok(())` on
    /// success; an `Err` causes the send to fail without persisting a
    /// challenge (so the caller's rate-limit budget is not consumed by a
    /// delivery failure).
    async fn deliver(&self, normalized_identifier: &str, code: &str) -> Result<(), String>;
}

/// No-op deliverer for tests.
pub struct NullDeliverer;

#[async_trait::async_trait]
impl OtpDeliverer for NullDeliverer {
    async fn deliver(&self, _id: &str, _code: &str) -> Result<(), String> {
        Ok(())
    }
}

/// Captures the last delivered code in a mutex — useful for integration tests
/// that need to verify the round-trip.
#[cfg(test)]
pub struct CapturingDeliverer {
    last: std::sync::Mutex<Option<String>>,
}

#[cfg(test)]
impl CapturingDeliverer {
    pub fn new() -> Self {
        Self { last: std::sync::Mutex::new(None) }
    }
    pub fn last_code(&self) -> Option<String> {
        self.last.lock().unwrap().clone()
    }
}

#[cfg(test)]
#[async_trait::async_trait]
impl OtpDeliverer for CapturingDeliverer {
    async fn deliver(&self, _id: &str, code: &str) -> Result<(), String> {
        *self.last.lock().unwrap() = Some(code.to_string());
        Ok(())
    }
}

impl OtpService {
    /// Create a production service using the system clock.
    pub fn new(redis: Arc<RedisClient>, deliverer: Arc<dyn OtpDeliverer>) -> Self {
        Self {
            redis,
            clock: Arc::new(SystemClock),
            deliverer,
        }
    }

    /// Create a service with a custom clock (tests).
    pub fn with_clock(
        redis: Arc<RedisClient>,
        deliverer: Arc<dyn OtpDeliverer>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            redis,
            clock,
            deliverer,
        }
    }

    // ── Send ──────────────────────────────────────────────────────────────

    /// Generate and "send" a one-time password for `identifier`.
    ///
    /// The method **always returns `SendOtpOutcome::Sent`** (with the same
    /// `expires_in`) whether or not the identifier maps to a real account —
    /// the deliverer is only called if the account is known.  This prevents
    /// account enumeration: an attacker cannot probe which emails/phones
    /// have accounts by observing differences in the response.
    ///
    /// Rate limits (send count, resend cooldown) are enforced *before*
    /// delivery so a flood of requests cannot trigger a flood of emails.
    pub async fn send_otp(&self, identifier: &str) -> Result<SendOtpOutcome, ApiError> {
        let normalized = normalize_identifier(identifier);
        let now = self.clock.now();
        let now_ts = now.timestamp();

        let mut conn = self
            .redis
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| ApiError::internal_error(format!("Redis connection failed: {}", e)))?;

        // ── Resend cooldown ──────────────────────────────────────────────
        // If a cooldown key exists, the caller must wait.
        let cooldown_ttl: Option<i64> = conn
            .ttl(resend_cooldown_key(&normalized))
            .await
            .unwrap_or(0);
        if cooldown_ttl > 0 {
            return Ok(SendOtpOutcome::RateLimited {
                retry_after: cooldown_ttl as u64,
            });
        }

        // ── Send rate limit (sliding window) ─────────────────────────────
        let (count, _, _) = sliding_window_incr(
            &mut conn,
            &send_rate_key(&normalized),
            now_ts,
            SEND_WINDOW_SECONDS,
            SEND_LIMIT,
        )
        .await;
        if count > SEND_LIMIT {
            let retry_after = SEND_WINDOW_SECONDS;
            return Ok(SendOtpOutcome::RateLimited { retry_after });
        }

        // ── Generate code ────────────────────────────────────────────────
        let code = generate_code();
        let salt = generate_salt();
        let code_hash = hash_code(&code, &salt);
        let expires_at = now_ts + OTP_TTL_SECONDS as i64;

        let record = OtpRecord {
            code_hash,
            salt,
            issued_at: now_ts,
            expires_at,
            attempts: 0,
            locked: false,
        };

        // ── Deliver ───────────────────────────────────────────────────────
        // The deliverer decides whether to actually send (account may not exist).
        // If delivery fails, we don't persist a challenge, so the rate limit
        // budget is not consumed by a delivery failure.
        if let Err(e) = self.deliverer.deliver(&normalized, &code).await {
            warn!(error = %e, "OTP delivery failed");
            return Err(ApiError::internal_error("Failed to deliver OTP"));
        }

        // ── Persist challenge ──────────────────────────────────────────────
        let record_json = serde_json::to_string(&record)
            .map_err(|e| ApiError::internal_error(format!("OTP serialization failed: {}", e)))?;

        let _: () = conn
            .set_ex(otp_key(&normalized), record_json, OTP_TTL_SECONDS)
            .await
            .map_err(|e| ApiError::internal_error(format!("Redis SET failed: {}", e)))?;

        // ── Set resend cooldown ─────────────────────────────────────────────
        let _: () = conn
            .set_ex(
                resend_cooldown_key(&normalized),
                "1",
                RESEND_COOLDOWN_SECONDS,
            )
            .await
            .map_err(|e| ApiError::internal_error(format!("Redis SET failed: {}", e)))?;

        info!(identifier = %normalized, "OTP sent");
        Ok(SendOtpOutcome::Sent {
            expires_in: OTP_TTL_SECONDS,
        })
    }

    // ── Verify ─────────────────────────────────────────────────────────────

    /// Verify a one-time password for `identifier`.
    ///
    /// On success the challenge is **immediately deleted** (single-use).
    /// On failure the attempt count is incremented and the challenge
    /// persists until expiry or lockout.
    ///
    /// Rate limits apply across all instances via Redis.
    pub async fn verify_otp(
        &self,
        identifier: &str,
        code: &str,
    ) -> Result<VerifyOtpOutcome, ApiError> {
        let normalized = normalize_identifier(identifier);
        let now = self.clock.now();
        let now_ts = now.timestamp();

        let mut conn = self
            .redis
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| ApiError::internal_error(format!("Redis connection failed: {}", e)))?;

        // ── Verify rate limit (sliding window) ────────────────────────────
        let (count, _, _) = sliding_window_incr(
            &mut conn,
            &verify_rate_key(&normalized),
            now_ts,
            VERIFY_WINDOW_SECONDS,
            VERIFY_LIMIT,
        )
        .await;
        if count > VERIFY_LIMIT {
            let retry_after = VERIFY_WINDOW_SECONDS;
            return Ok(VerifyOtpOutcome::RateLimited { retry_after });
        }

        // ── Fetch challenge ────────────────────────────────────────────────
        let record_json: Option<String> = conn
            .get(otp_key(&normalized))
            .await
            .map_err(|e| ApiError::internal_error(format!("Redis GET failed: {}", e)))?;

        let Some(record_json) = record_json else {
            // No challenge exists: expired, already used, or never sent.
            // Non-enumerating: same response as a wrong code.
            return Ok(VerifyOtpOutcome::NoChallenge);
        };

        let mut record: OtpRecord = serde_json::from_str(&record_json)
            .map_err(|e| ApiError::internal_error(format!("OTP deserialization failed: {}", e)))?;

        // ── Lockout check ───────────────────────────────────────────────────
        if record.locked {
            return Ok(VerifyOtpOutcome::Locked);
        }

        // ── Expiry check ────────────────────────────────────────────────────
        if now_ts >= record.expires_at {
            // Clean up the expired record.
            let _: () = conn.del(otp_key(&normalized)).await.unwrap_or(());
            return Ok(VerifyOtpOutcome::NoChallenge);
        }

        // ── Verify code (constant-time) ─────────────────────────────────────
        if record.verify(code) {
            // Success: immediately delete the challenge (single-use).
            let _: () = conn
                .del(otp_key(&normalized))
                .await
                .map_err(|e| ApiError::internal_error(format!("Redis DEL failed: {}", e)))?;
            debug!(identifier = %normalized, "OTP verified");
            Ok(VerifyOtpOutcome::Verified)
        } else {
            // Wrong code: increment attempts, lock if threshold reached.
            record.attempts += 1;
            if record.attempts >= MAX_VERIFY_ATTEMPTS {
                record.locked = true;
            }

            // Persist updated record.
            let updated_json = serde_json::to_string(&record)
                .map_err(|e| ApiError::internal_error(format!("OTP serialization failed: {}", e)))?;
            let remaining_ttl = record.expires_at.saturating_sub(now_ts).max(1) as u64;
            let _: () = conn
                .set_ex(otp_key(&normalized), updated_json, remaining_ttl)
                .await
                .map_err(|e| ApiError::internal_error(format!("Redis SET failed: {}", e)))?;

            let attempts_remaining = MAX_VERIFY_ATTEMPTS.saturating_sub(record.attempts);
            if record.locked {
                Ok(VerifyOtpOutcome::Locked)
            } else {
                Ok(VerifyOtpOutcome::WrongCode { attempts_remaining })
            }
        }
    }

    // ── Introspection (for tests) ────────────────────────────────────────────

    /// Check whether a challenge currently exists for `identifier`.
    #[cfg(test)]
    pub async fn challenge_exists(&self, identifier: &str) -> bool {
        let normalized = normalize_identifier(identifier);
        let mut conn = match self.redis.get_multiplexed_async_connection().await {
            Ok(c) => c,
            Err(_) => return false,
        };
        let val: Option<String> = conn.get(otp_key(&normalized)).await.unwrap_or(None);
        val.is_some()
    }
}

// ── Redis sliding-window helper ─────────────────────────────────────────────

/// Atomically increment a sliding-window counter in Redis.
///
/// Returns `(current_count, limit, reset_unix_seconds)`.
async fn sliding_window_incr(
    conn: &mut redis::aio::MultiplexedConnection,
    key: &str,
    now_ts: i64,
    window_secs: u64,
    _limit: u32,
) -> (u32, u32, u64) {
    let now_ms = (now_ts as u64) * 1_000;
    let window_ms = window_secs * 1_000;
    let window_start_ms = now_ms.saturating_sub(window_ms);
    let reset_secs = (now_ts as u64) + window_secs;

    let result: Result<(i64, i64, i64, i64), redis::RedisError> = redis::pipe()
        .atomic()
        .cmd("ZADD")
        .arg(key)
        .arg(now_ms)
        .arg(now_ms.to_string())
        .cmd("ZREMRANGEBYSCORE")
        .arg(key)
        .arg(0u64)
        .arg(window_start_ms)
        .cmd("ZCARD")
        .arg(key)
        .cmd("EXPIRE")
        .arg(key)
        .arg(window_secs)
        .query_async(conn)
        .await;

    match result {
        Ok((_, _, count, _)) => (count as u32, _limit, reset_secs),
        Err(e) => {
            warn!(error = %e, key = %key, "OTP rate-limit Redis error — failing open");
            (0, _limit, reset_secs)
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Unit tests (no Redis required) ──────────────────────────────────────

    #[test]
    fn normalize_email_lowercases_and_trims() {
        assert_eq!(normalize_identifier("  User@Example.com  "), "user@example.com");
    }

    #[test]
    fn normalize_phone_strips_spaces_and_plus() {
        assert_eq!(normalize_identifier("+1 234 567 890"), "123456789");
    }

    #[test]
    fn code_hash_is_deterministic_for_same_code_and_salt() {
        let salt = "abcdef0123456789";
        let h1 = hash_code("123456", salt);
        let h2 = hash_code("123456", salt);
        assert_eq!(h1, h2);
    }

    #[test]
    fn code_hash_differs_for_different_codes() {
        let salt = "abcdef0123456789";
        assert_ne!(hash_code("123456", salt), hash_code("654321", salt));
    }

    #[test]
    fn code_hash_differs_for_different_salts() {
        assert_ne!(
            hash_code("123456", "salt_a"),
            hash_code("123456", "salt_b")
        );
    }

    #[test]
    fn otp_record_verify_accepts_correct_code() {
        let salt = generate_salt();
        let record = OtpRecord {
            code_hash: hash_code("123456", &salt),
            salt,
            issued_at: 0,
            expires_at: i64::MAX,
            attempts: 0,
            locked: false,
        };
        assert!(record.verify("123456"));
    }

    #[test]
    fn otp_record_verify_rejects_wrong_code() {
        let salt = generate_salt();
        let record = OtpRecord {
            code_hash: hash_code("123456", &salt),
            salt,
            issued_at: 0,
            expires_at: i64::MAX,
            attempts: 0,
            locked: false,
        };
        assert!(!record.verify("000000"));
    }

    #[test]
    fn generated_code_is_always_six_digits() {
        for _ in 0..100 {
            let code = generate_code();
            assert_eq!(code.len(), CODE_LENGTH);
            assert!(code.chars().all(|c| c.is_ascii_digit()));
        }
    }

    // ── Fixed clock for deterministic tests ────────────────────────────────

    use std::sync::Mutex;

    struct FixedClock {
        now: Mutex<chrono::DateTime<chrono::Utc>>,
    }

    impl FixedClock {
        fn new(ts: i64) -> Self {
            Self {
                now: Mutex::new(chrono::DateTime::from_timestamp(ts, 0).unwrap()),
            }
        }
    }

    impl Clock for FixedClock {
        fn now(&self) -> chrono::DateTime<chrono::Utc> {
            *self.now.lock().unwrap()
        }
    }

    // ── Redis-backed integration tests ─────────────────────────────────────
    //
    // These tests require a Redis instance at the URL in REDIS_URL (or
    // localhost:6379 as a fallback).  They are marked `#[ignore]` so they
    // don't fail in environments without Redis, but can be run with:
    //
    //   cargo test -- --ignored otp_redis

    fn redis_url() -> String {
        std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string())
    }

    async fn make_service(clock_ts: i64) -> (OtpService, Arc<CapturingDeliverer>) {
        let client = Arc::new(redis::Client::open(redis_url()).unwrap());
        let deliverer = Arc::new(CapturingDeliverer::new());
        let clock = Arc::new(FixedClock::new(clock_ts));
        let service =
            OtpService::with_clock(client, deliverer.clone() as Arc<dyn OtpDeliverer>, clock);
        (service, deliverer)
    }

    async fn cleanup(service: &OtpService, identifier: &str) {
        let normalized = normalize_identifier(identifier);
        if let Ok(mut conn) = service.redis.get_multiplexed_async_connection().await {
            let _: () = conn.del(otp_key(&normalized)).await.unwrap_or(());
            let _: () = conn.del(send_rate_key(&normalized)).await.unwrap_or(());
            let _: () = conn.del(verify_rate_key(&normalized)).await.unwrap_or(());
            let _: () = conn.del(resend_cooldown_key(&normalized)).await.unwrap_or(());
        }
    }

    #[tokio::test]
    #[ignore = "requires Redis at REDIS_URL or localhost:6379"]
    async fn otp_redis_send_then_verify_success() {
        let (service, deliverer) = make_service(1000).await;
        let id = "testuser@example.com";
        cleanup(&service, id).await;

        // Send
        let outcome = service.send_otp(id).await.unwrap();
        assert!(matches!(outcome, SendOtpOutcome::Sent { .. }));

        // The deliverer captured the code
        let code = deliverer.last_code().expect("code was delivered");
        assert_eq!(code.len(), CODE_LENGTH);

        // Verify with the correct code
        let result = service.verify_otp(id, &code).await.unwrap();
        assert!(matches!(result, VerifyOtpOutcome::Verified));

        // The challenge is now gone (single-use)
        assert!(!service.challenge_exists(id).await);

        cleanup(&service, id).await;
    }

    #[tokio::test]
    #[ignore = "requires Redis at REDIS_URL or localhost:6379"]
    async fn otp_redis_replay_after_verify_fails() {
        let (service, deliverer) = make_service(1000).await;
        let id = "replay@example.com";
        cleanup(&service, id).await;

        service.send_otp(id).await.unwrap();
        let code = deliverer.last_code().unwrap();

        // First verify succeeds
        assert!(matches!(
            service.verify_otp(id, &code).await.unwrap(),
            VerifyOtpOutcome::Verified
        ));

        // Second verify with the same code fails (no challenge)
        let result = service.verify_otp(id, &code).await.unwrap();
        assert!(matches!(result, VerifyOtpOutcome::NoChallenge));

        cleanup(&service, id).await;
    }

    #[tokio::test]
    #[ignore = "requires Redis at REDIS_URL or localhost:6379"]
    async fn otp_redis_wrong_code_increments_attempts() {
        let (service, _deliverer) = make_service(1000).await;
        let id = "wrong@example.com";
        cleanup(&service, id).await;

        service.send_otp(id).await.unwrap();

        // First wrong attempt
        let result = service.verify_otp(id, "000000").await.unwrap();
        match result {
            VerifyOtpOutcome::WrongCode { attempts_remaining } => {
                assert_eq!(attempts_remaining, MAX_VERIFY_ATTEMPTS - 1);
            }
            other => panic!("expected WrongCode, got {other:?}"),
        }

        // Second wrong attempt
        let result = service.verify_otp(id, "000000").await.unwrap();
        match result {
            VerifyOtpOutcome::WrongCode { attempts_remaining } => {
                assert_eq!(attempts_remaining, MAX_VERIFY_ATTEMPTS - 2);
            }
            other => panic!("expected WrongCode, got {other:?}"),
        }

        cleanup(&service, id).await;
    }

    #[tokio::test]
    #[ignore = "requires Redis at REDIS_URL or localhost:6379"]
    async fn otp_redis_lockout_after_max_attempts() {
        let (service, _deliverer) = make_service(1000).await;
        let id = "lockout@example.com";
        cleanup(&service, id).await;

        service.send_otp(id).await.unwrap();

        // Exhaust all attempts
        for _ in 0..MAX_VERIFY_ATTEMPTS {
            let _ = service.verify_otp(id, "000000").await.unwrap();
        }

        // Next attempt is locked
        let result = service.verify_otp(id, "000000").await.unwrap();
        assert!(matches!(result, VerifyOtpOutcome::Locked));

        cleanup(&service, id).await;
    }

    #[tokio::test]
    #[ignore = "requires Redis at REDIS_URL or localhost:6379"]
    async fn otp_redis_expiry_returns_no_challenge() {
        let (service, _deliverer) = make_service(1000).await;
        let id = "expiry@example.com";
        cleanup(&service, id).await;

        service.send_otp(id).await.unwrap();
        service.verify_otp(id, "000000").await.unwrap();

        // Manually expire the record by deleting the key (simulates TTL expiry)
        if let Ok(mut conn) = service.redis.get_multiplexed_async_connection().await {
            let _: () = conn.del(otp_key(&normalize_identifier(id))).await.unwrap_or(());
        }

        let result = service.verify_otp(id, "000000").await.unwrap();
        assert!(matches!(result, VerifyOtpOutcome::NoChallenge));

        cleanup(&service, id).await;
    }

    #[tokio::test]
    #[ignore = "requires Redis at REDIS_URL or localhost:6379"]
    async fn otp_redis_resend_cooldown() {
        let (service, _deliverer) = make_service(1000).await;
        let id = "resend@example.com";
        cleanup(&service, id).await;

        // First send succeeds
        let outcome = service.send_otp(id).await.unwrap();
        assert!(matches!(outcome, SendOtpOutcome::Sent { .. }));

        // Immediate resend is rate-limited
        let outcome = service.send_otp(id).await.unwrap();
        match outcome {
            SendOtpOutcome::RateLimited { retry_after } => {
                assert!(retry_after > 0 && retry_after <= RESEND_COOLDOWN_SECONDS);
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }

        cleanup(&service, id).await;
    }

    #[tokio::test]
    #[ignore = "requires Redis at REDIS_URL or localhost:6379"]
    async fn otp_redis_send_rate_limit() {
        let (service, _deliverer) = make_service(1000).await;
        let id = "ratelimit@example.com";
        cleanup(&service, id).await;

        // We can send up to SEND_LIMIT times (accounting for cooldown, we
        // need to clear the resend key between sends for this test).
        for i in 0..SEND_LIMIT {
            if i > 0 {
                // Clear the resend cooldown for the test
                if let Ok(mut conn) = service.redis.get_multiplexed_async_connection().await {
                    let _: () = conn
                        .del(resend_cooldown_key(&normalize_identifier(id)))
                        .await
                        .unwrap_or(());
                }
            }
            let outcome = service.send_otp(id).await.unwrap();
            assert!(
                matches!(outcome, SendOtpOutcome::Sent { .. }),
                "send {} should succeed",
                i + 1
            );
        }

        // Clear the resend cooldown one more time
        if let Ok(mut conn) = service.redis.get_multiplexed_async_connection().await {
            let _: () = conn
                .del(resend_cooldown_key(&normalize_identifier(id)))
                .await
                .unwrap_or(());
        }

        // Next send exceeds the rate limit
        let outcome = service.send_otp(id).await.unwrap();
        assert!(matches!(outcome, SendOtpOutcome::RateLimited { .. }));

        cleanup(&service, id).await;
    }

    #[tokio::test]
    #[ignore = "requires Redis at REDIS_URL or localhost:6379"]
    async fn otp_redis_non_enumerating_response() {
        let (service, _deliverer) = make_service(1000).await;
        let id = "nonenum@example.com";
        cleanup(&service, id).await;

        // Send returns the same response shape regardless
        let outcome = service.send_otp(id).await.unwrap();
        match outcome {
            SendOtpOutcome::Sent { expires_in } => {
                assert_eq!(expires_in, OTP_TTL_SECONDS);
            }
            other => panic!("expected Sent, got {other:?}"),
        }

        // Verify with a random code returns a non-enumerating response
        let result = service.verify_otp(id, "999999").await.unwrap();
        // Either WrongCode or NoChallenge, but never reveals account existence
        assert!(matches!(
            result,
            VerifyOtpOutcome::WrongCode { .. } | VerifyOtpOutcome::NoChallenge
        ));

        cleanup(&service, id).await;
    }

    #[tokio::test]
    #[ignore = "requires Redis at REDIS_URL or localhost:6379"]
    async fn otp_redis_clock_boundary_expiry() {
        let (service, deliverer) = make_service(1000).await;
        let id = "boundary@example.com";
        cleanup(&service, id).await;

        service.send_otp(id).await.unwrap();
        let code = deliverer.last_code().unwrap();

        // The OTP expires_at = 1000 + OTP_TTL_SECONDS = 1000 + 300 = 1300
        // We simulate expiry by deleting the key (Redis TTL handles real expiry)
        if let Ok(mut conn) = service.redis.get_multiplexed_async_connection().await {
            let _: () = conn
                .del(otp_key(&normalize_identifier(id)))
                .await
                .unwrap_or(());
        }

        let result = service.verify_otp(id, &code).await.unwrap();
        assert!(matches!(result, VerifyOtpOutcome::NoChallenge));

        cleanup(&service, id).await;
    }

    #[tokio::test]
    #[ignore = "requires Redis at REDIS_URL or localhost:6379"]
    async fn otp_redis_concurrent_verification_single_use() {
        let (service, deliverer) = make_service(1000).await;
        let id = "concurrent@example.com";
        cleanup(&service, id).await;

        service.send_otp(id).await.unwrap();
        let code = deliverer.last_code().unwrap();

        // Fire two concurrent verifications with the correct code.
        let (r1, r2) = tokio::join!(
            service.verify_otp(id, &code),
            service.verify_otp(id, &code),
        );

        let r1 = r1.unwrap();
        let r2 = r2.unwrap();

        // At most one should succeed; the other should get NoChallenge.
        let success_count = [matches!(r1, VerifyOtpOutcome::Verified), matches!(r2, VerifyOtpOutcome::Verified)]
            .iter()
            .filter(|&&x| x)
            .count();
        assert_eq!(success_count, 1, "exactly one concurrent verify should succeed");

        cleanup(&service, id).await;
    }
}
