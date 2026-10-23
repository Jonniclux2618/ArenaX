#![allow(dead_code)]

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

const PRIZE_WEIGHT_DENOMINATOR: u32 = 10_000;

/// V2 payout event; amounts are token base units and settlement_id is the stable escrow ID.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrizeDistributionEventV2 {
    pub settlement_id: String,
    pub asset: String,
    pub gross_amount: i128,
    pub fee_bps: u32,
    pub fee_amount: i128,
    pub rounding_remainder: i128,
    pub treasury: String,
    pub winners: Vec<String>,
    pub weights: Vec<u32>,
    pub amounts: Vec<i128>,
}

impl PrizeDistributionEventV2 {
    pub fn parse_and_validate(payload: &str) -> Result<Self, String> {
        let event: Self = serde_json::from_str(payload).map_err(|error| error.to_string())?;
        event.validate()?;
        Ok(event)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.settlement_id.is_empty() || self.asset.is_empty() || self.treasury.is_empty() {
            return Err("settlement, asset, and treasury identifiers are required".to_string());
        }
        if self.winners.is_empty()
            || self.winners.len() > 64
            || self.winners.len() != self.weights.len()
            || self.winners.len() != self.amounts.len()
        {
            return Err("winner, weight, and amount lists must be non-empty and aligned".to_string());
        }
        if self.fee_bps > PRIZE_WEIGHT_DENOMINATOR {
            return Err("fee exceeds the basis-point denominator".to_string());
        }

        if self.gross_amount <= 0 || self.fee_amount < 0 || self.rounding_remainder < 0 {
            return Err("gross amount must be positive and other amounts non-negative".to_string());
        }
        let expected_fee = self.gross_amount
            .checked_mul(i128::from(self.fee_bps))
            .ok_or_else(|| "fee calculation overflow".to_string())?
            / i128::from(PRIZE_WEIGHT_DENOMINATOR);
        if self.fee_amount != expected_fee {
            return Err("fee amount does not match fee basis points".to_string());
        }

        let mut seen_winners = HashSet::with_capacity(self.winners.len());
        let mut total_weight = 0u32;
        let mut total_winners = 0i128;
        let mut rounded_winners = 0i128;
        let mut final_base_amount = 0i128;
        let distributable = self.gross_amount - self.fee_amount;
        for index in 0..self.winners.len() {
            if self.winners[index].is_empty() || !seen_winners.insert(&self.winners[index]) {
                return Err("winner identifiers must be non-empty and unique".to_string());
            }
            let weight = self.weights[index];
            if weight == 0 {
                return Err("winner weights must be non-zero".to_string());
            }
            total_weight = total_weight
                .checked_add(weight)
                .ok_or_else(|| "weight sum overflow".to_string())?;
            let amount = self.amounts[index];
            if amount < 0 {
                return Err("amounts must not be negative".to_string());
            }
            let expected_amount = distributable
                .checked_mul(i128::from(weight))
                .ok_or_else(|| "winner payout calculation overflow".to_string())?
                / i128::from(PRIZE_WEIGHT_DENOMINATOR);
            rounded_winners = rounded_winners
                .checked_add(expected_amount)
                .ok_or_else(|| "rounded payout total overflow".to_string())?;
            if index + 1 == self.winners.len() {
                final_base_amount = expected_amount;
            } else if amount != expected_amount {
                return Err("winner amount does not match its configured weight".to_string());
            }
            total_winners = total_winners
                .checked_add(amount)
                .ok_or_else(|| "winner payout total overflow".to_string())?;
        }
        if total_weight != PRIZE_WEIGHT_DENOMINATOR {
            return Err("winner weights must sum to 10,000".to_string());
        }
        let expected_remainder = distributable - rounded_winners;
        if self.rounding_remainder != expected_remainder
            || self.amounts[self.amounts.len() - 1]
                != final_base_amount + expected_remainder
        {
            return Err("rounding remainder does not match winner allocations".to_string());
        }
        if self.fee_amount
            .checked_add(total_winners)
            != Some(self.gross_amount)
        {
            return Err("payout amounts do not reconcile to the gross pool".to_string());
        }
        Ok(())
    }
}

/// Settlement status for tracking the lifecycle of a reward settlement
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SettlementStatus {
    Pending,
    Submitted,
    Confirmed,
    Failed,
    DeadLettered,
}

impl std::fmt::Display for SettlementStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SettlementStatus::Pending => write!(f, "pending"),
            SettlementStatus::Submitted => write!(f, "submitted"),
            SettlementStatus::Confirmed => write!(f, "confirmed"),
            SettlementStatus::Failed => write!(f, "failed"),
            SettlementStatus::DeadLettered => write!(f, "dead_lettered"),
        }
    }
}

/// Reward settlement record as specified in issue #46
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RewardSettlement {
    #[serde(rename = "matchId")]
    pub match_id: String,
    pub winner: String,
    pub amount: String,
    pub asset: String,
    #[serde(rename = "txHash", skip_serializing_if = "Option::is_none")]
    pub tx_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<SettlementStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settled_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub retry_count: u32,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_retry_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub dead_lettered: bool,
    #[serde(default)]
    pub webhook_sent: bool,
    #[serde(default)]
    pub idempotency_key: String,
}

fn default_max_retries() -> u32 {
    3
}

impl RewardSettlement {
    pub fn new(match_id: String, winner: String, amount: String, asset: String) -> Self {
        let idempotency_key = format!("{}:{}:{}", match_id, winner, asset);
        Self {
            match_id,
            winner,
            amount,
            asset,
            tx_hash: None,
            status: Some(SettlementStatus::Pending),
            created_at: Some(Utc::now()),
            settled_at: None,
            retry_count: 0,
            max_retries: 3,
            next_retry_at: None,
            dead_lettered: false,
            webhook_sent: false,
            idempotency_key,
        }
    }

    /// Check if settlement is already confirmed on-chain
    pub fn is_settled(&self) -> bool {
        matches!(self.status, Some(SettlementStatus::Confirmed))
    }
}
