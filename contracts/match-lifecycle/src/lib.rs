#![no_std]

use arenax_events::{match_contract, match_lifecycle as lifecycle_events};
use soroban_sdk::{contract, contractimpl, contracttype, Address, BytesN, Env};

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MatchState {
    Created,
    Started,
    Completed,
    Disputed,
    DisputeResolved,
    Finalized,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MatchRecord {
    pub match_id: BytesN<32>,
    pub player_a: Address,
    pub player_b: Address,
    pub state: MatchState,
    pub winner: Option<Address>,
    pub created_at: u64,
    pub started_at: Option<u64>,
    pub completed_at: Option<u64>,
    pub finalized_at: Option<u64>,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
enum DataKey {
    Admin,
    Match(BytesN<32>),
}

#[contract]
pub struct MatchLifecycleContract;

#[contractimpl]
impl MatchLifecycleContract {
    pub fn initialize(env: Env, admin: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic!("already initialized");
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
    }

    pub fn create_match(
        env: Env,
        operator: Address,
        match_id: BytesN<32>,
        player_a: Address,
        player_b: Address,
    ) {
        Self::require_admin(&env, &operator);
        if player_a == player_b {
            panic!("players must be distinct");
        }
        let key = DataKey::Match(match_id.clone());
        if env.storage().persistent().has(&key) {
            panic!("match already exists");
        }

        let record = MatchRecord {
            match_id: match_id.clone(),
            player_a: player_a.clone(),
            player_b: player_b.clone(),
            state: MatchState::Created,
            winner: None,
            created_at: env.ledger().timestamp(),
            started_at: None,
            completed_at: None,
            finalized_at: None,
        };
        env.storage().persistent().set(&key, &record);
        match_contract::emit_match_created(&env, &match_id, &player_a, &player_b);
    }

    pub fn start_match(env: Env, actor: Address, match_id: BytesN<32>) {
        actor.require_auth();
        let mut record = Self::load_match(&env, &match_id);
        Self::require_participant_or_admin(&env, &record, &actor);
        if record.state != MatchState::Created {
            panic!("invalid match transition");
        }
        let now = env.ledger().timestamp();
        record.state = MatchState::Started;
        record.started_at = Some(now);
        env.storage()
            .persistent()
            .set(&DataKey::Match(match_id.clone()), &record);
        match_contract::emit_match_started(&env, &match_id, now);
    }

    pub fn complete_match(
        env: Env,
        operator: Address,
        match_id: BytesN<32>,
        winner: Address,
    ) {
        Self::require_admin(&env, &operator);
        let mut record = Self::load_match(&env, &match_id);
        if record.state != MatchState::Started {
            panic!("invalid match transition");
        }
        Self::require_participant(&record, &winner);
        let now = env.ledger().timestamp();
        record.state = MatchState::Completed;
        record.winner = Some(winner.clone());
        record.completed_at = Some(now);
        env.storage()
            .persistent()
            .set(&DataKey::Match(match_id.clone()), &record);
        match_contract::emit_match_completed(&env, &match_id, &winner);
    }

    pub fn raise_dispute(env: Env, actor: Address, match_id: BytesN<32>) {
        actor.require_auth();
        let mut record = Self::load_match(&env, &match_id);
        Self::require_participant(&record, &actor);
        Self::set_disputed(&env, &mut record, &match_id);
    }

    pub fn raise_dispute_by_operator(
        env: Env,
        operator: Address,
        actor: Address,
        match_id: BytesN<32>,
    ) {
        Self::require_admin(&env, &operator);
        let mut record = Self::load_match(&env, &match_id);
        Self::require_participant(&record, &actor);
        Self::set_disputed(&env, &mut record, &match_id);
    }

    fn set_disputed(env: &Env, record: &mut MatchRecord, match_id: &BytesN<32>) {
        if record.state != MatchState::Completed {
            panic!("invalid match transition");
        }
        record.state = MatchState::Disputed;
        env.storage()
            .persistent()
            .set(&DataKey::Match(match_id.clone()), record);
        match_contract::emit_match_disputed(env, match_id);
    }

    pub fn resolve_dispute(
        env: Env,
        operator: Address,
        match_id: BytesN<32>,
        winner: Address,
    ) {
        Self::require_admin(&env, &operator);
        let mut record = Self::load_match(&env, &match_id);
        if record.state != MatchState::Disputed {
            panic!("match is not disputed");
        }
        Self::require_participant(&record, &winner);
        record.state = MatchState::DisputeResolved;
        record.winner = Some(winner.clone());
        env.storage()
            .persistent()
            .set(&DataKey::Match(match_id.clone()), &record);
        match_contract::emit_match_resolved(&env, &match_id, &winner);
    }

    pub fn finalize_match(env: Env, operator: Address, match_id: BytesN<32>) {
        Self::require_admin(&env, &operator);
        let mut record = Self::load_match(&env, &match_id);
        if record.state != MatchState::Completed && record.state != MatchState::DisputeResolved {
            panic!("invalid match transition");
        }
        let winner = record.winner.clone().expect("match result is missing");
        let now = env.ledger().timestamp();
        record.state = MatchState::Finalized;
        record.finalized_at = Some(now);
        env.storage()
            .persistent()
            .set(&DataKey::Match(match_id.clone()), &record);
        lifecycle_events::emit_match_finalized(&env, &match_id, &winner, now);
    }

    pub fn get_match(env: Env, match_id: BytesN<32>) -> Option<MatchRecord> {
        env.storage().persistent().get(&DataKey::Match(match_id))
    }

    pub fn get_admin(env: Env) -> Address {
        env.storage().instance().get(&DataKey::Admin).expect("not initialized")
    }

    fn load_match(env: &Env, match_id: &BytesN<32>) -> MatchRecord {
        env.storage()
            .persistent()
            .get(&DataKey::Match(match_id.clone()))
            .expect("match not found")
    }

    fn require_admin(env: &Env, operator: &Address) {
        operator.require_auth();
        let admin: Address = env.storage().instance().get(&DataKey::Admin).expect("not initialized");
        if *operator != admin {
            panic!("operator is not authorized");
        }
    }

    fn require_participant(record: &MatchRecord, actor: &Address) {
        if *actor != record.player_a && *actor != record.player_b {
            panic!("address is not a match participant");
        }
    }

    fn require_participant_or_admin(env: &Env, record: &MatchRecord, actor: &Address) {
        let admin: Address = env.storage().instance().get(&DataKey::Admin).expect("not initialized");
        if *actor != admin {
            Self::require_participant(record, actor);
        }
    }
}

#[cfg(test)]
mod test;