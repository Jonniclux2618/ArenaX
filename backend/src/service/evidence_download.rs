//! Short-lived, one-object dispute evidence download capabilities.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::{collections::HashSet, sync::RwLock};
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

const DEFAULT_TTL_SECONDS: i64 = 300;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct EvidenceDownloadClaims {
    pub jti: Uuid,
    pub viewer_id: Uuid,
    pub case_id: Uuid,
    pub object_key: String,
    pub expires_at: i64,
}

pub struct EvidenceDownloadCapability {
    secret: Vec<u8>,
    ttl_seconds: i64,
    redeemed: RwLock<HashSet<Uuid>>,
}

impl EvidenceDownloadCapability {
    pub fn from_env() -> Result<Self, String> {
        let secret = std::env::var("EVIDENCE_DOWNLOAD_SECRET")
            .map_err(|_| "EVIDENCE_DOWNLOAD_SECRET is not configured".to_string())?;
        if secret.len() < 32 {
            return Err("EVIDENCE_DOWNLOAD_SECRET must contain at least 32 bytes".to_string());
        }
        Ok(Self {
            secret: secret.into_bytes(),
            ttl_seconds: DEFAULT_TTL_SECONDS,
            redeemed: RwLock::new(HashSet::new()),
        })
    }

    pub fn issue(
        &self,
        viewer_id: Uuid,
        case_id: Uuid,
        object_key: &str,
    ) -> Result<String, String> {
        if object_key.is_empty() || object_key.contains('\\') || object_key.contains('\0') {
            return Err("invalid evidence object key".to_string());
        }
        let claims = EvidenceDownloadClaims {
            jti: Uuid::new_v4(),
            viewer_id,
            case_id,
            object_key: object_key.to_string(),
            expires_at: chrono::Utc::now().timestamp() + self.ttl_seconds,
        };
        let payload = serde_json::to_vec(&claims)
            .map_err(|_| "could not serialize evidence download capability".to_string())?;
        let mut mac = HmacSha256::new_from_slice(&self.secret)
            .map_err(|_| "could not initialize evidence download signer".to_string())?;
        mac.update(&payload);
        let signature = mac.finalize().into_bytes();
        Ok(format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(payload),
            URL_SAFE_NO_PAD.encode(signature)
        ))
    }

    pub fn verify(&self, capability: &str, now: i64) -> Result<EvidenceDownloadClaims, String> {
        let mut parts = capability.split('.');
        let payload_encoded = parts.next().ok_or_else(|| "invalid capability".to_string())?;
        let signature_encoded = parts.next().ok_or_else(|| "invalid capability".to_string())?;
        if parts.next().is_some() {
            return Err("invalid capability".to_string());
        }
        let payload = URL_SAFE_NO_PAD
            .decode(payload_encoded)
            .map_err(|_| "invalid capability".to_string())?;
        let signature = URL_SAFE_NO_PAD
            .decode(signature_encoded)
            .map_err(|_| "invalid capability".to_string())?;
        let mut mac = HmacSha256::new_from_slice(&self.secret)
            .map_err(|_| "could not initialize evidence download signer".to_string())?;
        mac.update(&payload);
        mac.verify_slice(&signature)
            .map_err(|_| "invalid evidence download capability".to_string())?;
        let claims: EvidenceDownloadClaims = serde_json::from_slice(&payload)
            .map_err(|_| "invalid evidence download capability".to_string())?;
        if claims.expires_at < now {
            return Err("evidence download capability has expired".to_string());
        }
        Ok(claims)
    }

    pub fn consume(&self, claims: &EvidenceDownloadClaims) -> Result<(), String> {
        let mut redeemed = self.redeemed.write().map_err(|_| "capability state unavailable".to_string())?;
        if !redeemed.insert(claims.jti) {
            return Err("evidence download capability has already been used".to_string());
        }
        Ok(())
    }
}
