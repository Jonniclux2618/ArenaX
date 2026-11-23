use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use hmac::{Hmac, Mac};
use redis::{aio::ConnectionManager, RedisError};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone)]
pub struct StepUpSecret(pub String);

#[derive(Deserialize)]
struct StepUpClaims {
    #[serde(rename = "actorId")]
    actor_id: String,
    action: String,
    #[serde(rename = "operationHash")]
    operation_hash: String,
    #[serde(rename = "sessionHash")]
    session_hash: String,
    #[serde(rename = "expiresAt")]
    expires_at: i64,
    id: String,
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(object) => {
            let mut keys: Vec<_> = object.keys().collect();
            keys.sort();
            format!(
                "{{{}}}",
                keys.into_iter()
                    .map(|key| format!(
                        "{}:{}",
                        serde_json::to_string(key).expect("serializing a JSON key cannot fail"),
                        canonical_json(object.get(key).expect("key came from this object"))
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
        Value::Array(values) => format!(
            "[{}]",
            values.iter().map(canonical_json).collect::<Vec<_>>().join(",")
        ),
        _ => serde_json::to_string(value).expect("serializing a JSON value cannot fail"),
    }
}

fn fingerprint(action: &str, operation: &Value) -> String {
    let mut digest = Sha256::new();
    digest.update(format!("{action}\n{}", canonical_json(operation)));
    hex::encode(digest.finalize())
}

fn digest(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

fn unauthorized() -> crate::api_error::ApiError {
    crate::api_error::ApiError::unauthorized("Invalid, expired, or mismatched step-up proof")
}

pub async fn verify_withdrawal_proof(
    proof: &str,
    actor_id: &str,
    bearer_token: &str,
    operation: &Value,
    secret: &str,
    redis: &mut ConnectionManager,
) -> Result<(), crate::api_error::ApiError> {
    let (encoded, signature) = proof.split_once('.').ok_or_else(unauthorized)?;
    let signature = URL_SAFE_NO_PAD.decode(signature).map_err(|_| unauthorized())?;
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|_| crate::api_error::ApiError::ServiceUnavailable("Step-up verification unavailable".into()))?;
    mac.update(encoded.as_bytes());
    mac.verify_slice(&signature).map_err(|_| unauthorized())?;

    let claims: StepUpClaims = serde_json::from_slice(
        &URL_SAFE_NO_PAD.decode(encoded).map_err(|_| unauthorized())?,
    )
    .map_err(|_| unauthorized())?;
    let now = chrono::Utc::now().timestamp();
    if claims.actor_id != actor_id
        || claims.action != "wallet.withdraw"
        || claims.operation_hash != fingerprint("wallet.withdraw", operation)
        || claims.session_hash != digest(bearer_token)
        || claims.expires_at <= now
    {
        return Err(unauthorized());
    }

    let ttl = claims.expires_at.saturating_sub(now);
    let consumed: Option<String> = redis::cmd("SET")
        .arg(format!("step-up:used:{}", claims.id))
        .arg(actor_id)
        .arg("EX")
        .arg(ttl)
        .arg("NX")
        .query_async(redis)
        .await
        .map_err(|error: RedisError| {
            tracing::error!(%error, "Step-up proof Redis store unavailable");
            crate::api_error::ApiError::ServiceUnavailable(
                "Step-up verification store unavailable".into(),
            )
        })?;
    if consumed.as_deref() != Some("OK") {
        return Err(unauthorized());
    }
    tracing::info!(
        actor_id,
        action = "wallet.withdraw",
        proof_id = %claims.id,
        "Step-up proof consumed"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_fingerprint_binds_the_action_amount_and_destination() {
        let operation = serde_json::json!({
            "method": "POST",
            "path": "/api/wallet/withdraw",
            "query": {},
            "body": {
                "amount": "25.00",
                "currency": "NGN",
                "destination": "account-a",
                "payment_method": "bank_transfer"
            }
        });
        let original = super::fingerprint("wallet.withdraw", &operation);
        assert_ne!(original, super::fingerprint("api_key.create", &operation));

        let mut changed_amount = operation.clone();
        changed_amount["body"]["amount"] = Value::String("26.00".into());
        assert_ne!(original, super::fingerprint("wallet.withdraw", &changed_amount));

        let mut changed_destination = operation.clone();
        changed_destination["body"]["destination"] = Value::String("account-b".into());
        assert_ne!(original, super::fingerprint("wallet.withdraw", &changed_destination));
    }
}
