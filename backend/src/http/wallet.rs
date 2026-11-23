use actix_web::{web, HttpResponse, Result};
use serde::Deserialize;
use rust_decimal::prelude::ToPrimitive;
use sqlx::PgPool;
use uuid::Uuid;

use crate::api_error::ApiError;
use crate::auth::middleware::ClaimsExt;
use crate::contract_errors::{self, ContractError, RecoveryClass};
use crate::models::{
    DepositRequest, PaginatedResponse, PaginationParams, TransactionResponse, TransactionStatus,
    TransactionType, WalletResponse, WithdrawalRequest,
};
use crate::service::WalletService;

/// Map a service-layer error to a stable public contract error response.
///
/// Known errors resolve through the canonical catalog; unknown errors are
/// logged with transaction context (never signing material) and returned as a
/// distinguishable `UNKNOWN_CONTRACT_ERROR` code.
fn map_service_error(err: &crate::service::wallet_service::WalletError, context: &str) -> ApiError {
    let contract_error = ContractError::from_wallet_error(err);
    if matches!(contract_error, ContractError::Unknown { .. }) {
        tracing::error!(
            context = %context,
            error = %err,
            "Unmapped wallet/contract error encountered"
        );
    }
    contract_error.to_api_error()
}

/// Build the JSON body describing a contract error for clients.
fn contract_error_body(err: &ContractError) -> serde_json::Value {
    serde_json::json!({
        "code": err.code(),
        "message": err.safe_message(),
        "recovery": err.recovery().as_str(),
        "retryable": matches!(err.recovery(), RecoveryClass::Retry),
    })
}

#[derive(Deserialize)]
pub struct PaymentVerificationRequest {
    pub reference: String,
    pub provider: String,
}

pub async fn get_wallet(
    pool: web::Data<PgPool>,
    req: actix_web::HttpRequest,
) -> Result<HttpResponse, ApiError> {
    let user_id = req
        .user_id()
        .ok_or_else(|| ApiError::unauthorized("User not authenticated"))?;

    let wallet = sqlx::query_as!(
        crate::models::Wallet,
        r#"
        SELECT * FROM wallets
        WHERE user_id = $1
        "#,
        user_id
    )
    .fetch_optional(pool.get_ref())
    .await?
    .ok_or_else(|| ApiError::not_found("Wallet not found"))?;

    Ok(HttpResponse::Ok().json(WalletResponse::from(wallet)))
}

pub async fn get_transaction_history(
    pool: web::Data<PgPool>,
    req: actix_web::HttpRequest,
    query: web::Query<PaginationParams>,
) -> Result<HttpResponse, ApiError> {
    let user_id = req
        .user_id()
        .ok_or_else(|| ApiError::unauthorized("User not authenticated"))?;

    let limit = query.resolved_limit();
    let offset = query.sql_offset();

    let service = WalletService::new(pool.get_ref().clone().into(), None);
    let (transactions, total) = service
        .get_transaction_history_paginated(user_id, limit, offset)
        .await
        .map_err(|e| map_service_error(&e, "get_transaction_history"))?;

    let data: Vec<TransactionResponse> = transactions
        .into_iter()
        .map(|t| TransactionResponse {
            id: t.id,
            transaction_type: t.transaction_type,
            amount: t.amount,
            currency: t.currency,
            status: t.status,
            reference: t.reference,
            description: t.description,
            stellar_transaction_id: t.stellar_transaction_id.map(|u| u.to_string()),
            created_at: t.created_at,
            completed_at: t.completed_at,
        })
        .collect();

    Ok(HttpResponse::Ok().json(PaginatedResponse::new(data, total, &query)))
}

pub async fn initiate_deposit(
    pool: web::Data<PgPool>,
    req: actix_web::HttpRequest,
    body: web::Json<DepositRequest>,
) -> Result<HttpResponse, ApiError> {
    let user_id = req
        .user_id()
        .ok_or_else(|| ApiError::unauthorized("User not authenticated"))?;

    let amount = body.amount;
    if amount <= rust_decimal::Decimal::ZERO {
        return Err(ApiError::bad_request("Amount must be positive"));
    }

    let service = WalletService::new(pool.get_ref().clone().into(), None);
    let transaction = service
        .create_transaction(
            user_id,
            TransactionType::Deposit,
            amount.to_i64().ok_or_else(|| ApiError::bad_request("Invalid amount"))?,
            body.currency.clone(),
            format!("Wallet deposit via {}", body.payment_method),
            None,
        )
        .await
        .map_err(|e| map_service_error(&e, "initiate_deposit"))?;

    Ok(HttpResponse::Ok().json(serde_json::json!({
        "transaction_id": transaction.id,
        "reference": transaction.reference,
        "status": "pending",
        "amount": amount,
        "currency": body.currency,
        "payment_method": body.payment_method,
        "message": "Deposit initiated. Complete payment to finalize."
    })))
}

pub async fn verify_deposit(
    pool: web::Data<PgPool>,
    req: actix_web::HttpRequest,
    body: web::Json<PaymentVerificationRequest>,
) -> Result<HttpResponse, ApiError> {
    let user_id = req
        .user_id()
        .ok_or_else(|| ApiError::unauthorized("User not authenticated"))?;

    let service = WalletService::new(pool.get_ref().clone().into(), None);

    let transaction = service
        .get_transaction_by_reference(&body.reference)
        .await
        .map_err(|e| map_service_error(&e, "verify_deposit:lookup"))?;

    // Duplicate delivery: an already-completed transaction is idempotent and
    // must not be re-verified or re-applied.
    if transaction.status == TransactionStatus::Completed {
        return Ok(HttpResponse::Ok().json(serde_json::json!({
            "status": "completed",
            "duplicate": true,
            "transaction": transaction
        })));
    }

    // The active gateway comes from PAYMENT_PROVIDER (#1069); a provider
    // that isn't enabled verifies as false, as an unknown one always did.
    let verified = service
        .verify_payment(&body.provider, &body.reference, transaction.amount.to_i64().unwrap_or(0))
        .await
        .map_err(|e| map_service_error(&e, "verify_deposit:verify"))?;

    if verified {
        service
            .update_transaction_status(transaction.id, TransactionStatus::Completed)
            .await
            .map_err(|e| map_service_error(&e, "verify_deposit:finalize"))?;
    }

    Ok(HttpResponse::Ok().json(serde_json::json!({
        "status": if verified { "completed" } else { "failed" },
        "verified": verified,
        "duplicate": false
    })))
}

pub async fn initiate_withdrawal(
    pool: web::Data<PgPool>,
    redis: web::Data<redis::aio::ConnectionManager>,
    step_up_secret: web::Data<crate::security::step_up::StepUpSecret>,
    req: actix_web::HttpRequest,
    body: web::Json<WithdrawalRequest>,
) -> Result<HttpResponse, ApiError> {
    let user_id = req
        .user_id()
        .ok_or_else(|| ApiError::unauthorized("User not authenticated"))?;

    let amount = body.amount;
    if amount <= rust_decimal::Decimal::ZERO {
        return Err(ApiError::bad_request("Amount must be positive"));
    }

    let proof = req
        .headers()
        .get("x-step-up-proof")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::unauthorized("Recent step-up proof required"))?;
    let bearer_token = req
        .headers()
        .get("Authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or_else(|| ApiError::unauthorized("Authentication required"))?;
    if !req.query_string().is_empty() {
        return Err(ApiError::bad_request("Withdrawal does not accept query parameters"));
    }
    let operation = serde_json::json!({
        "method": "POST",
        "path": req.path(),
        "query": {},
        "body": {
            "amount": amount.to_string(),
            "currency": body.currency,
            "destination": body.destination,
            "payment_method": body.payment_method
        }
    });
    let mut redis_connection = redis.get_ref().clone();
    crate::security::step_up::verify_withdrawal_proof(
        proof,
        &user_id.to_string(),
        bearer_token,
        &operation,
        &step_up_secret.0,
        &mut redis_connection,
    )
    .await?;

    let service = WalletService::new(pool.get_ref().clone().into(), None);

    let wallet = service
        .get_wallet(user_id)
        .await
        .map_err(|e| map_service_error(&e, "initiate_withdrawal:wallet"))?;

    let available_balance = match body.currency.as_str() {
        "NGN" => wallet.balance_ngn.unwrap_or(0),
        "XLM" => wallet.balance_xlm.unwrap_or(0),
        "ARENAX_TOKEN" => wallet.balance_arenax_tokens.unwrap_or(0),
        _ => 0,
    };

    let amount_in_smallest_unit = match body.currency.as_str() {
        "NGN" | "ARENAX_TOKEN" => amount.to_i64().unwrap_or(0),
        "XLM" => amount.to_i64().unwrap_or(0) / 1_000_000,
        _ => amount.to_i64().unwrap_or(0),
    };

    if available_balance < amount_in_smallest_unit {
        return Err(ApiError::bad_request(format!(
            "Insufficient {} balance. Available: {}",
            body.currency, available_balance
        )));
    }

    let transaction = service
        .create_transaction(
            user_id,
            TransactionType::Withdrawal,
            amount_in_smallest_unit,
            body.currency.clone(),
            format!("Withdrawal to {}", body.destination),
            None,
        )
        .await
        .map_err(|e| map_service_error(&e, "initiate_withdrawal:create"))?;

    Ok(HttpResponse::Ok().json(serde_json::json!({
        "transaction_id": transaction.id,
        "reference": transaction.reference,
        "status": "pending",
        "amount": amount,
        "currency": body.currency,
        "destination": body.destination,
        "payment_method": body.payment_method,
        "message": "Withdrawal initiated. Processing may take a few minutes."
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::wallet_service::WalletError;

    #[test]
    fn known_errors_map_to_stable_codes() {
        let err = WalletError::InsufficientBalance {
            required: 100,
            available: 10,
        };
        let contract_error = ContractError::from_wallet_error(&err);
        let body = contract_error_body(&contract_error);
        assert_eq!(body["code"], contract_error.code());
        assert_eq!(body["recovery"], contract_error.recovery().as_str());
        assert!(body["message"].as_str().unwrap().len() > 0);
    }

    #[test]
    fn unknown_error_is_distinguishable_and_retryable() {
        let err = WalletError::Internal("boom: secret=abc".to_string());
        let contract_error = ContractError::from_wallet_error(&err);
        let body = contract_error_body(&contract_error);
        assert_eq!(body["code"], "UNKNOWN_CONTRACT_ERROR");
        assert_eq!(body["retryable"], true);
        // Safe message must not leak raw internal detail.
        assert!(!body["message"].as_str().unwrap().contains("secret"));
    }

    #[test]
    fn retry_classification_is_exposed() {
        let err = WalletError::NetworkTimeout;
        let contract_error = ContractError::from_wallet_error(&err);
        let body = contract_error_body(&contract_error);
        assert_eq!(body["retryable"], true);
        assert_eq!(body["recovery"], RecoveryClass::Retry.as_str());
    }

    #[test]
    fn failure_classification_is_not_retryable() {
        let err = WalletError::InsufficientBalance {
            required: 100,
            available: 10,
        };
        let contract_error = ContractError::from_wallet_error(&err);
        let body = contract_error_body(&contract_error);
        assert_eq!(body["retryable"], false);
        assert_eq!(body["recovery"], RecoveryClass::Manual.as_str());
    }
}