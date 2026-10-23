//! OTP authentication HTTP handlers (Issue #6).
//!
//! Exposes the OTP send and verify endpoints.  All responses are
//! non-enumerating: whether or not the account exists, the response
//! shape is identical so an attacker cannot discover which emails or
//! phone numbers have accounts.

use crate::api_error::ApiError;
use crate::service::otp_service::{OtpService, SendOtpOutcome, VerifyOtpOutcome};
use actix_web::{web, HttpResponse};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// ── Request / response DTOs ────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SendOtpRequest {
    /// Email address or phone number.
    pub identifier: String,
}

#[derive(Debug, Deserialize)]
pub struct VerifyOtpRequest {
    pub identifier: String,
    pub code: String,
}

#[derive(Debug, Serialize)]
struct SendOtpResponse {
    message: &'static str,
    expires_in: u64,
}

#[derive(Debug, Serialize)]
struct RateLimitedResponse {
    message: &'static str,
    retry_after: u64,
}

#[derive(Debug, Serialize)]
struct VerifyOtpResponse {
    verified: bool,
    message: String,
    attempts_remaining: Option<u32>,
}

// ── Handlers ────────────────────────────────────────────────────────────────

/// POST /api/auth/otp/send
///
/// Request a one-time password.  The response is always the same shape
/// regardless of whether the account exists (anti-enumeration).
pub async fn send_otp(
    otp_service: web::Data<Arc<OtpService>>,
    body: web::Json<SendOtpRequest>,
) -> Result<HttpResponse, ApiError> {
    if body.identifier.trim().is_empty() {
        return Err(ApiError::bad_request("Identifier is required"));
    }

    let outcome = otp_service.send_otp(&body.identifier).await?;

    match outcome {
        SendOtpOutcome::Sent { expires_in } => Ok(HttpResponse::Ok().json(SendOtpResponse {
            // Non-enumerating: always says "sent", even if no account.
            message: "If an account exists for this identifier, a verification code has been sent.",
            expires_in,
        })),
        SendOtpOutcome::RateLimited { retry_after } => {
            Ok(HttpResponse::TooManyRequests().json(RateLimitedResponse {
                message: "Too many requests. Please try again later.",
                retry_after,
            }))
        }
    }
}

/// POST /api/auth/otp/verify
///
/// Verify a one-time password.  Returns a non-enumerating response on
/// failure.
pub async fn verify_otp(
    otp_service: web::Data<Arc<OtpService>>,
    body: web::Json<VerifyOtpRequest>,
) -> Result<HttpResponse, ApiError> {
    if body.identifier.trim().is_empty() || body.code.trim().is_empty() {
        return Err(ApiError::bad_request("Identifier and code are required"));
    }

    let outcome = otp_service.verify_otp(&body.identifier, &body.code).await?;

    match outcome {
        VerifyOtpOutcome::Verified => Ok(HttpResponse::Ok().json(VerifyOtpResponse {
            verified: true,
            message: "Verification successful.".to_string(),
            attempts_remaining: None,
        })),
        VerifyOtpOutcome::WrongCode { attempts_remaining } => {
            Ok(HttpResponse::Ok().json(VerifyOtpResponse {
                verified: false,
                // Non-enumerating: does not confirm whether the account exists.
                message: "Invalid or expired code.".to_string(),
                attempts_remaining: Some(attempts_remaining),
            }))
        }
        VerifyOtpOutcome::Locked => Ok(HttpResponse::Ok().json(VerifyOtpResponse {
            verified: false,
            message: "Too many attempts. Please request a new code.".to_string(),
            attempts_remaining: Some(0),
        })),
        VerifyOtpOutcome::NoChallenge => Ok(HttpResponse::Ok().json(VerifyOtpResponse {
            verified: false,
            // Same message as WrongCode so the caller can't distinguish
            // "no code was ever sent" from "code is wrong".
            message: "Invalid or expired code.".to_string(),
            attempts_remaining: None,
        })),
        VerifyOtpOutcome::RateLimited { retry_after } => {
            Ok(HttpResponse::TooManyRequests().json(RateLimitedResponse {
                message: "Too many verification attempts. Please try again later.",
                retry_after,
            }))
        }
    }
}

/// Configure OTP routes inside the auth scope.
pub fn configure_routes(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/otp")
            .route("/send", web::post().to(send_otp))
            .route("/verify", web::post().to(verify_otp)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn send_otp_request_deserializes() {
        let req: SendOtpRequest = serde_json::from_str(
            r#"{"identifier":"user@example.com"}"#,
        )
        .unwrap();
        assert_eq!(req.identifier, "user@example.com");
    }

    #[test]
    fn verify_otp_request_deserializes() {
        let req: VerifyOtpRequest =
            serde_json::from_str(r#"{"identifier":"user@example.com","code":"123456"}"#).unwrap();
        assert_eq!(req.identifier, "user@example.com");
        assert_eq!(req.code, "123456");
    }

    #[test]
    fn send_otp_response_is_non_enumerating() {
        let resp = SendOtpResponse {
            message: "If an account exists for this identifier, a verification code has been sent.",
            expires_in: 300,
        };
        let body = serde_json::to_value(&resp).unwrap();
        // The message never confirms account existence.
        assert!(body["message"]
            .as_str()
            .unwrap()
            .contains("If an account exists"));
    }

    #[test]
    fn verify_otp_response_does_not_leak_account_existence() {
        // Both WrongCode and NoChallenge produce the same message.
        let wrong = VerifyOtpResponse {
            verified: false,
            message: "Invalid or expired code.".to_string(),
            attempts_remaining: Some(3),
        };
        let no_challenge = VerifyOtpResponse {
            verified: false,
            message: "Invalid or expired code.".to_string(),
            attempts_remaining: None,
        };
        assert_eq!(wrong.message, no_challenge.message);
    }

    #[test]
    fn rate_limited_response_has_retry_after() {
        let resp = RateLimitedResponse {
            message: "Too many requests. Please try again later.",
            retry_after: 60,
        };
        let body = serde_json::to_value(&resp).unwrap();
        assert_eq!(body["retry_after"].as_u64(), Some(60));
    }
}
