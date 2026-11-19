use actix_web::{web, HttpResponse, Result};
use serde::Deserialize;
use rust_decimal::prelude::ToPrimitive;
use sqlx::PgPool;
use uuid::Uuid;

use crate::api_error::ApiError;
use crate::auth::middleware::ClaimsExt;
use crate::models::{
    DepositRequest, PaginatedResponse, PaginationParams, TransactionResponse, TransactionStatus,
    TransactionType, WalletResponse, WithdrawalRequest,
};
use crate::service::WalletService;

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
        .map_err(|e| ApiError::internal_error(format!("Failed to fetch transactions: {}", e)))?;

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
        .await?;

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
        .await?;

    if transaction.status == TransactionStatus::Completed {
        return Ok(HttpResponse::Ok().json(serde_json::json!({
            "status": "completed",
            "transaction": transaction
        })));
    }

    // The active gateway comes from PAYMENT_PROVIDER (#1069); a provider
    // that isn't enabled verifies as false, as an unknown one always did.
    let verified = service
        .verify_payment(&body.provider, &body.reference, transaction.amount.to_i64().unwrap_or(0))
        .await?;

    if verified {
        service
            .update_transaction_status(transaction.id, TransactionStatus::Completed)
            .await?;
    }

    Ok(HttpResponse::Ok().json(serde_json::json!({
        "status": if verified { "completed" } else { "failed" },
        "verified": verified
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

    let wallet = service.get_wallet(user_id).await.map_err(|e| match e {
        crate::service::wallet_service::WalletError::InsufficientBalance { required, available } => {
            ApiError::bad_request(format!(
                "Insufficient balance: required {}, available {}",
                required, available
            ))
        }
        _ => ApiError::not_found("Wallet not found"),
    })?;

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
        .await?;

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