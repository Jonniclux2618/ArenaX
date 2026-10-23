use soroban_sdk::{contractevent, Address, BytesN, Env, Vec};

pub const NAMESPACE: &str = "ArenaXPrizeDistribution";
pub const VERSION: &str = "v2";

#[contractevent(topics = ["ArenaXPrize_v2", "CREATED"])]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PoolCreated {
    pub pool_id: u64,
    pub match_id: BytesN<32>,
    pub asset: Address,
    pub amount_locked: i128,
}

#[contractevent(topics = ["ArenaXPrize_v2", "LOCKED"])]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PoolLocked {
    pub pool_id: u64,
    pub amount_locked: i128,
}

#[contractevent(topics = ["ArenaXPrize_v2", "EXECUTED"])]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PayoutExecuted {
    pub settlement_id: BytesN<32>,
    pub asset: Address,
    pub gross_amount: i128,
    pub fee_bps: u32,
    pub fee_amount: i128,
    pub rounding_remainder: i128,
    pub treasury: Address,
    pub winners: Vec<Address>,
    pub weights: Vec<u32>,
    pub amounts: Vec<i128>,
}

#[contractevent(topics = ["ArenaXPrize_v2", "HELD"])]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PayoutHeld {
    pub pool_id: u64,
    pub match_id: BytesN<32>,
}

#[contractevent(topics = ["ArenaXPrize_v2", "RELEASED"])]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PayoutReleased {
    pub pool_id: u64,
    pub match_id: BytesN<32>,
}

pub fn emit_pool_created(
    env: &Env,
    pool_id: u64,
    match_id: &BytesN<32>,
    asset: &Address,
    amount_locked: i128,
) {
    PoolCreated {
        pool_id,
        match_id: match_id.clone(),
        asset: asset.clone(),
        amount_locked,
    }
    .publish(env);
}

pub fn emit_pool_locked(env: &Env, pool_id: u64, amount_locked: i128) {
    PoolLocked {
        pool_id,
        amount_locked,
    }
    .publish(env);
}

pub fn emit_payout_executed_v2(
    env: &Env,
    settlement_id: &BytesN<32>,
    asset: &Address,
    gross_amount: i128,
    fee_bps: u32,
    fee_amount: i128,
    rounding_remainder: i128,
    treasury: &Address,
    winners: &Vec<Address>,
    weights: &Vec<u32>,
    amounts: &Vec<i128>,
) {
    PayoutExecuted {
        settlement_id: settlement_id.clone(),
        asset: asset.clone(),
        gross_amount,
        fee_bps,
        fee_amount,
        rounding_remainder,
        treasury: treasury.clone(),
        winners: winners.clone(),
        weights: weights.clone(),
        amounts: amounts.clone(),
    }
    .publish(env);
}

pub fn emit_payout_held(env: &Env, pool_id: u64, match_id: &BytesN<32>) {
    PayoutHeld {
        pool_id,
        match_id: match_id.clone(),
    }
    .publish(env);
}

pub fn emit_payout_released(env: &Env, pool_id: u64, match_id: &BytesN<32>) {
    PayoutReleased {
        pool_id,
        match_id: match_id.clone(),
    }
    .publish(env);
}
