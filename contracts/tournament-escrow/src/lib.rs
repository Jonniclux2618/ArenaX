#![no_std]

use soroban_sdk::{
    contract, contractimpl, contracttype, symbol_short, token, Address, BytesN, Env, Map, String,
    Vec,
};

const MAX_PARTICIPANTS: u32 = 64;
const MAX_EVIDENCE_REF_BYTES: u32 = 256;
const DEFAULT_DISPUTE_WINDOW: u64 = 604_800;
const WEIGHT_DENOMINATOR: u32 = 10_000;

/// Maximum duration for emergency pause: 7 days. Enforces bounded response without permanent lockout (#57).
pub const MAX_PAUSE_DURATION: u64 = 604_800;

/// Maximum duration for emergency pause: 7 days. Enforces bounded response without permanent lockout (#57).
pub const MAX_PAUSE_DURATION: u64 = 604_800;

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EscrowStatus {
    Active,
    Disputed,
    Released,
    Refunded,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PauseState {
    pub paused: bool,
    pub paused_at: u64,
    pub expires_at: u64,
    pub paused_by: Address,
    pub reason: Symbol,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Escrow {
    pub asset: Address,
    pub entry_amount: i128,
    pub total_held: i128,
    pub participants: Vec<Address>,
    pub deposits: Map<Address, i128>,
    pub status: EscrowStatus,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisputeMetadata {
    pub evidence_ref: String,
    pub deadline: u64,
    pub resolved: bool,
    pub decision: Option<Address>,
}

#[contracttype]
#[derive(Clone)]
enum DataKey {
    Admin,
    Resolver,
    Treasury,
    FeeBps,
    Escrow(BytesN<32>),
    Dispute(BytesN<32>),
}

#[contract]
pub struct TournamentEscrow;

#[contractimpl]
impl TournamentEscrow {
    pub fn initialize(env: Env, admin: Address, resolver: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic!("already initialized");
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::Resolver, &resolver);
        env.storage().instance().set(&DataKey::Treasury, &admin);
        env.storage().instance().set(&DataKey::FeeBps, &0u32);
        env.events()
            .publish((symbol_short!("INIT"),), (admin, resolver));
    }

    pub fn set_resolver(env: Env, admin: Address, resolver: Address) {
        Self::require_admin(&env, &admin);
        env.storage().instance().set(&DataKey::Resolver, &resolver);
        env.events()
            .publish((symbol_short!("RESOLVER"),), (admin, resolver));
    }

    pub fn configure_distribution(
        env: Env,
        admin: Address,
        treasury: Address,
        fee_bps: u32,
    ) {
        Self::require_admin(&env, &admin);
        if fee_bps > WEIGHT_DENOMINATOR {
            panic!("fee exceeds denominator");
        }
        env.storage().instance().set(&DataKey::Treasury, &treasury);
        env.storage().instance().set(&DataKey::FeeBps, &fee_bps);
        env.events().publish(
            (symbol_short!("DIST_CFG"),),
            (treasury, fee_bps),
        );
    }

    pub fn create_escrow(
        env: Env,
        resolver: Address,
        tournament_id: BytesN<32>,
        asset: Address,
        entry_amount: i128,
    ) {
        Self::require_not_paused(&env);
        Self::require_resolver(&env, &resolver);
        if entry_amount <= 0 {
            panic!("entry amount must be positive");
        }
        let key = DataKey::Escrow(tournament_id.clone());
        if env.storage().persistent().has(&key) {
            panic!("escrow already exists");
        }

        let escrow = Escrow {
            asset: asset.clone(),
            entry_amount,
            total_held: 0,
            participants: Vec::new(&env),
            deposits: Map::new(&env),
            status: EscrowStatus::Active,
        };
        env.storage().persistent().set(&key, &escrow);
        env.events().publish(
            (symbol_short!("CREATED"), tournament_id),
            (asset, entry_amount),
        );
    }

    pub fn deposit(env: Env, tournament_id: BytesN<32>, participant: Address, amount: i128) {
        Self::require_not_paused(&env);
        participant.require_auth();
        if amount <= 0 {
            panic!("amount must be positive");
        }
        let key = DataKey::Escrow(tournament_id.clone());
        let mut escrow: Escrow = env
            .storage()
            .persistent()
            .get(&key)
            .expect("escrow not found");
        if escrow.status != EscrowStatus::Active {
            panic!("escrow is not accepting deposits");
        }
        if amount != escrow.entry_amount {
            panic!("deposit must match entry amount");
        }
        if escrow.deposits.contains_key(participant.clone()) {
            panic!("participant already deposited");
        }
        if escrow.participants.len() >= MAX_PARTICIPANTS {
            panic!("participant limit reached");
        }
        let new_total = escrow
            .total_held
            .checked_add(amount)
            .expect("escrow total overflow");

        token::Client::new(&env, &escrow.asset).transfer(
            &participant,
            &env.current_contract_address(),
            &amount,
        );
        escrow.participants.push_back(participant.clone());
        escrow.deposits.set(participant.clone(), amount);
        escrow.total_held = new_total;
        env.storage().persistent().set(&key, &escrow);
        env.events().publish(
            (symbol_short!("DEPOSIT"), tournament_id),
            (participant, amount, new_total),
        );
    }

    /// User-fund exit during emergency or pause (#57).
    ///
    /// Allows participants to reclaim their own deposits even when the contract
    /// is paused or disputed, preventing permanent lockout.
    pub fn emergency_withdraw(env: Env, tournament_id: BytesN<32>, participant: Address) {
        participant.require_auth();

        let key = DataKey::Escrow(tournament_id.clone());
        let mut escrow: Escrow = env
            .storage()
            .persistent()
            .get(&key)
            .expect("escrow not found");

        if escrow.status != EscrowStatus::Active && escrow.status != EscrowStatus::Disputed {
            panic!("escrow is already settled");
        }

        let amount = escrow
            .deposits
            .get(participant.clone())
            .expect("participant has no deposit");

        if amount <= 0 {
            panic!("no refundable deposit");
        }

        // Deduct from total and remove deposit record
        escrow.deposits.remove(participant.clone());
        escrow.total_held = escrow.total_held.checked_sub(amount).expect("underflow");

        let mut new_participants = Vec::new(&env);
        for p in escrow.participants.iter() {
            if p != participant {
                new_participants.push_back(p);
            }
        }
        escrow.participants = new_participants;

        env.storage().persistent().set(&key, &escrow);

        token::Client::new(&env, &escrow.asset).transfer(
            &env.current_contract_address(),
            &participant,
            &amount,
        );

        env.events().publish(
            (symbol_short!("EMERG_EXIT"), tournament_id),
            (participant, amount),
        );
    }

    pub fn dispute(env: Env, resolver: Address, tournament_id: BytesN<32>) {
        Self::require_resolver(&env, &resolver);
        let deadline = env
            .ledger()
            .timestamp()
            .checked_add(DEFAULT_DISPUTE_WINDOW)
            .expect("dispute deadline overflow");
        Self::open_dispute(&env, &resolver, &tournament_id, String::from_str(&env, ""), deadline);
    }

    pub fn open_dispute_with_evidence(
        env: Env,
        resolver: Address,
        tournament_id: BytesN<32>,
        evidence_ref: String,
        deadline: u64,
    ) {
        Self::require_resolver(&env, &resolver);
        Self::open_dispute(&env, &resolver, &tournament_id, evidence_ref, deadline);
    }

    fn open_dispute(
        env: &Env,
        resolver: &Address,
        tournament_id: &BytesN<32>,
        evidence_ref: String,
        deadline: u64,
    ) {
        if evidence_ref.len() > MAX_EVIDENCE_REF_BYTES {
            panic!("evidence reference too long");
        }
        if deadline <= env.ledger().timestamp() {
            panic!("dispute deadline must be in the future");
        }
        let key = DataKey::Escrow(tournament_id.clone());
        let mut escrow: Escrow = env
            .storage()
            .persistent()
            .get(&key)
            .expect("escrow not found");
        if escrow.status != EscrowStatus::Active {
            panic!("escrow is not active");
        }
        escrow.status = EscrowStatus::Disputed;
        env.storage().persistent().set(&key, &escrow);
        env.storage().persistent().set(
            &DataKey::Dispute(tournament_id.clone()),
            &DisputeMetadata {
                evidence_ref: evidence_ref.clone(),
                deadline,
                resolved: false,
                decision: None,
            },
        );
        env.events().publish(
            (symbol_short!("DISPUTE_OPEN"), tournament_id.clone()),
            (evidence_ref, deadline, resolver.clone()),
        );
    }

    /// Settle to `winner`, or refund every participant when `winner` is `None`.
    /// A disputed escrow can only be settled by the configured resolver.
    /// Winner payouts are blocked while paused to prevent exploit drains (#57).
    pub fn resolve(
        env: Env,
        resolver: Address,
        tournament_id: BytesN<32>,
        winner: Option<Address>,
    ) {
        Self::require_resolver(&env, &resolver);
        let key = DataKey::Escrow(tournament_id.clone());
        let mut escrow: Escrow = env
            .storage()
            .persistent()
            .get(&key)
            .expect("escrow not found");
        if escrow.status != EscrowStatus::Active && escrow.status != EscrowStatus::Disputed {
            panic!("escrow is already settled");
        }
        let mut dispute = if escrow.status == EscrowStatus::Disputed {
            let metadata: DisputeMetadata = env
                .storage()
                .persistent()
                .get(&DataKey::Dispute(tournament_id.clone()))
                .expect("dispute metadata missing");
            if metadata.resolved {
                panic!("dispute is already resolved");
            }
            if env.ledger().timestamp() > metadata.deadline {
                panic!("dispute deadline passed");
            }
            Some(metadata)
        } else {
            None
        };
        let client = token::Client::new(&env, &escrow.asset);
        let contract_address = env.current_contract_address();
        if let Some(winner) = winner {
            if dispute.is_none() {
                panic!("winner payout requires a finalized dispute");
            }
            if escrow.total_held <= 0 {
                panic!("escrow has no deposits");
            }
            if !escrow.deposits.contains_key(winner.clone()) {
                panic!("winner is not a participant");
            }
            let gross_amount = escrow.total_held;
            let fee_bps: u32 = env.storage().instance().get(&DataKey::FeeBps).unwrap_or(0);
            let treasury: Address = env
                .storage()
                .instance()
                .get(&DataKey::Treasury)
                .expect("treasury missing");
            let fee_amount = gross_amount
                .checked_mul(fee_bps as i128)
                .expect("fee calculation overflow")
                / WEIGHT_DENOMINATOR as i128;
            let winner_amount = gross_amount - fee_amount;
            escrow.status = EscrowStatus::Released;
            escrow.total_held = 0;
            env.storage().persistent().set(&key, &escrow);
            if let Some(metadata) = dispute.as_mut() {
                metadata.resolved = true;
                metadata.decision = Some(winner.clone());
                env.storage().persistent().set(
                    &DataKey::Dispute(tournament_id.clone()),
                    metadata,
                );
                env.events().publish(
                    (symbol_short!("DISP_RESOLVED"), tournament_id.clone()),
                    (
                        Some(winner.clone()),
                        metadata.evidence_ref.clone(),
                        env.ledger().timestamp(),
                        resolver.clone(),
                    ),
                );
            }
            if fee_amount > 0 {
                client.transfer(&contract_address, &treasury, &fee_amount);
            }
            client.transfer(&contract_address, &winner, &winner_amount);
            let mut winners = Vec::new(&env);
            winners.push_back(winner.clone());
            let mut weights = Vec::new(&env);
            weights.push_back(WEIGHT_DENOMINATOR);
            let mut amounts = Vec::new(&env);
            amounts.push_back(winner_amount);
            arenax_events::prize_distribution::emit_payout_executed_v2(
                &env,
                &tournament_id,
                &escrow.asset,
                gross_amount,
                fee_bps,
                fee_amount,
                0,
                &treasury,
                &winners,
                &weights,
                &amounts,
            );
            env.events().publish(
                (symbol_short!("RELEASE"), tournament_id),
                (winner, gross_amount, resolver),
            );
        } else {
            let refund_total = escrow.total_held;
            escrow.status = EscrowStatus::Refunded;
            escrow.total_held = 0;
            env.storage().persistent().set(&key, &escrow);
            let mut index = 0;
            while index < escrow.participants.len() {
                let participant = escrow.participants.get(index).unwrap();
                let amount = escrow.deposits.get(participant.clone()).unwrap();
                client.transfer(&contract_address, &participant, &amount);
                env.events().publish(
                    (symbol_short!("REFUND"), tournament_id.clone()),
                    (participant, amount),
                );
                index += 1;
            }
            env.events().publish(
                (symbol_short!("REFUNDED"), tournament_id.clone()),
                (refund_total, resolver.clone()),
            );
            if let Some(metadata) = dispute.as_mut() {
                metadata.resolved = true;
                metadata.decision = None;
                env.storage().persistent().set(
                    &DataKey::Dispute(tournament_id.clone()),
                    metadata,
                );
                env.events().publish(
                    (symbol_short!("DISP_RESOLVED"), tournament_id),
                    (
                        Option::<Address>::None,
                        metadata.evidence_ref.clone(),
                        env.ledger().timestamp(),
                        resolver,
                    ),
                );
            }
        }
    }

    pub fn expire_dispute(env: Env, tournament_id: BytesN<32>) {
        let key = DataKey::Escrow(tournament_id.clone());
        let mut escrow: Escrow = env
            .storage()
            .persistent()
            .get(&key)
            .expect("escrow not found");
        if escrow.status != EscrowStatus::Disputed {
            panic!("escrow is not disputed");
        }
        let mut metadata: DisputeMetadata = env
            .storage()
            .persistent()
            .get(&DataKey::Dispute(tournament_id.clone()))
            .expect("dispute metadata missing");
        if metadata.resolved {
            panic!("dispute is already resolved");
        }
        if env.ledger().timestamp() <= metadata.deadline {
            panic!("dispute deadline has not passed");
        }
        metadata.resolved = true;
        metadata.decision = None;
        env.storage()
            .persistent()
            .set(&DataKey::Dispute(tournament_id.clone()), &metadata);
        let refund_total = escrow.total_held;
        escrow.status = EscrowStatus::Refunded;
        escrow.total_held = 0;
        env.storage().persistent().set(&key, &escrow);
        let client = token::Client::new(&env, &escrow.asset);
        let contract_address = env.current_contract_address();
        let mut index = 0;
        while index < escrow.participants.len() {
            let participant = escrow.participants.get(index).unwrap();
            let amount = escrow.deposits.get(participant.clone()).unwrap();
            client.transfer(&contract_address, &participant, &amount);
            index += 1;
        }
        env.events().publish(
            (symbol_short!("DISP_EXPIRED"), tournament_id.clone()),
            (refund_total, metadata.deadline),
        );
        env.events().publish(
            (symbol_short!("DISP_RESOLVED"), tournament_id.clone()),
            (
                Option::<Address>::None,
                metadata.evidence_ref,
                env.ledger().timestamp(),
                Option::<Address>::None,
            ),
        );
        env.events().publish(
            (symbol_short!("REFUNDED"), tournament_id),
            (refund_total, Option::<Address>::None),
        );
    }

    /// Distributes the held pool using 10,000-based weights. Integer dust is
    /// assigned to the last winner, so the policy is deterministic.
    pub fn distribute(
        env: Env,
        resolver: Address,
        tournament_id: BytesN<32>,
        winners: Vec<Address>,
        weights: Vec<u32>,
    ) {
        Self::require_resolver(&env, &resolver);
        if winners.is_empty() || winners.len() > MAX_PARTICIPANTS || winners.len() != weights.len() {
            panic!("invalid winner set");
        }
        let key = DataKey::Escrow(tournament_id.clone());
        let mut escrow: Escrow = env
            .storage()
            .persistent()
            .get(&key)
            .expect("escrow not found");
        if escrow.status != EscrowStatus::Active {
            panic!("escrow is not available for payout");
        }
        if escrow.total_held <= 0 {
            panic!("escrow has no deposits");
        }

        let mut total_weight = 0u32;
        let mut index = 0;
        while index < winners.len() {
            let winner = winners.get(index).unwrap();
            let weight = weights.get(index).unwrap();
            if weight == 0 || !escrow.deposits.contains_key(winner.clone()) {
                panic!("invalid winner or weight");
            }
            let mut prior = 0;
            while prior < index {
                if winners.get(prior).unwrap() == winner {
                    panic!("duplicate winner");
                }
                prior += 1;
            }
            total_weight = total_weight
                .checked_add(weight)
                .expect("weight sum overflow");
            index += 1;
        }
        if total_weight != WEIGHT_DENOMINATOR {
            panic!("weights must sum to denominator");
        }

        let treasury: Address = env.storage().instance().get(&DataKey::Treasury).expect("treasury missing");
        let fee_bps: u32 = env.storage().instance().get(&DataKey::FeeBps).unwrap_or(0);
        let gross_amount = escrow.total_held;
        let fee_amount = gross_amount
            .checked_mul(fee_bps as i128)
            .expect("fee calculation overflow")
            / WEIGHT_DENOMINATOR as i128;
        let distributable = gross_amount - fee_amount;
        let mut amounts = Vec::new(&env);
        let mut paid_to_winners = 0i128;
        index = 0;
        while index < winners.len() {
            let weight = weights.get(index).unwrap();
            let amount = distributable
                .checked_mul(weight as i128)
                .expect("payout calculation overflow")
                / WEIGHT_DENOMINATOR as i128;
            amounts.push_back(amount);
            paid_to_winners = paid_to_winners
                .checked_add(amount)
                .expect("payout total overflow");
            index += 1;
        }
        let rounding_remainder = distributable - paid_to_winners;
        let last_index = amounts.len() - 1;
        let final_amount = amounts.get(last_index).unwrap();
        amounts.set(
            last_index,
            final_amount
                .checked_add(rounding_remainder)
                .expect("final payout overflow"),
        );

        escrow.status = EscrowStatus::Released;
        escrow.total_held = 0;
        env.storage().persistent().set(&key, &escrow);
        let client = token::Client::new(&env, &escrow.asset);
        let contract_address = env.current_contract_address();
        if fee_amount > 0 {
            client.transfer(&contract_address, &treasury, &fee_amount);
        }
        index = 0;
        while index < winners.len() {
            let amount = amounts.get(index).unwrap();
            if amount > 0 {
                client.transfer(
                    &contract_address,
                    &winners.get(index).unwrap(),
                    &amount,
                );
            }
            index += 1;
        }
        arenax_events::prize_distribution::emit_payout_executed_v2(
            &env,
            &tournament_id,
            &escrow.asset,
            gross_amount,
            fee_bps,
            fee_amount,
            rounding_remainder,
            &treasury,
            &winners,
            &weights,
            &amounts,
        );
    }

    pub fn get_escrow(env: Env, tournament_id: BytesN<32>) -> Option<Escrow> {
        env.storage()
            .persistent()
            .get(&DataKey::Escrow(tournament_id))
    }

    pub fn get_dispute_metadata(env: Env, tournament_id: BytesN<32>) -> Option<DisputeMetadata> {
        env.storage()
            .persistent()
            .get(&DataKey::Dispute(tournament_id))
    }

    pub fn get_admin(env: Env) -> Address {
        env.storage().instance().get(&DataKey::Admin).unwrap()
    }

    fn check_is_paused(env: &Env) -> bool {
        if let Some(state) = env
            .storage()
            .instance()
            .get::<DataKey, PauseState>(&DataKey::PauseState)
        {
            if state.paused && env.ledger().timestamp() < state.expires_at {
                return true;
            }
        }
        false
    }

    fn require_not_paused(env: &Env) {
        if Self::check_is_paused(env) {
            panic!("contract is emergency paused");
        }
    }

    fn require_pause_authority(env: &Env, caller: &Address) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .expect("not initialized");
        let guardian: Option<Address> = env.storage().instance().get(&DataKey::Guardian);

        if *caller == admin {
            return;
        }
        if let Some(g) = guardian {
            if *caller == g {
                return;
            }
        }
        panic!("caller is not authorized to pause");
    }

    fn require_admin(env: &Env, admin: &Address) {
        admin.require_auth();
        let stored: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .expect("not initialized");
        if *admin != stored {
            panic!("caller is not admin");
        }
    }

    fn require_resolver(env: &Env, resolver: &Address) {
        resolver.require_auth();
        let stored: Address = env
            .storage()
            .instance()
            .get(&DataKey::Resolver)
            .expect("not initialized");
        if *resolver != stored {
            panic!("caller is not resolver");
        }
    }
}

#[cfg(test)]
mod test;
