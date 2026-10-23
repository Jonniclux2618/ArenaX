#![cfg(test)]

use super::*;
use soroban_sdk::{testutils::Address as _, Address, BytesN, Env, String, Vec};

fn setup(env: &Env) -> (UpgradeManagerClient<'_>, Address, Address) {
    env.mock_all_auths();
    let contract_id = env.register(UpgradeManager, ());
    let client = UpgradeManagerClient::new(env, &contract_id);
    let admin = Address::generate(env);
    let governor = Address::generate(env);

    let mut governors = Vec::new(env);
    governors.push_back(admin.clone());
    governors.push_back(governor.clone());

    client.initialize(&admin, &governors, &1);
    (client, admin, governor)
}

fn wasm_hash(env: &Env, byte: u8) -> BytesN<32> {
    BytesN::from_array(env, &[byte; 32])
}

#[test]
fn pause_round_trip() {
    let env = Env::default();
    let (client, admin, _governor) = setup(&env);

    assert!(!client.is_paused());
    client.set_paused(&admin, &true);
    assert!(client.is_paused());
    client.set_paused(&admin, &false);
    assert!(!client.is_paused());
}

#[test]
fn pause_by_non_admin_fails() {
    let env = Env::default();
    let (client, _admin, _governor) = setup(&env);

    let intruder = Address::generate(&env);
    assert_eq!(
        client.try_set_paused(&intruder, &true),
        Err(Ok(UpgradeError::NotAdmin))
    );
    // The flag must be untouched.
    assert!(!client.is_paused());
}

#[test]
fn proposal_blocked_while_paused() {
    let env = Env::default();
    let (client, admin, _governor) = setup(&env);

    client.set_paused(&admin, &true);

    let proposer = Address::generate(&env);
    assert_eq!(
        client.try_propose_upgrade(
            &proposer,
            &symbol_short!("token"),
            &wasm_hash(&env, 2),
            &String::from_str(&env, "paused"),
            &0u64,
        ),
        Err(Ok(UpgradeError::ContractPaused))
    );
}

#[test]
fn vote_blocked_while_paused() {
    let env = Env::default();
    let (client, admin, governor) = setup(&env);

    let proposer = Address::generate(&env);
    client.propose_upgrade(
        &proposer,
        &symbol_short!("token"),
        &wasm_hash(&env, 2),
        &String::from_str(&env, "v2"),
        &0,
    );

    client.set_paused(&admin, &true);

    assert_eq!(
        client.try_vote_upgrade(&governor, &1u32, &true),
        Err(Ok(UpgradeError::ContractPaused))
    );
}

#[test]
fn queries_work_while_paused() {
    let env = Env::default();
    let (client, admin, _governor) = setup(&env);

    let proposer = Address::generate(&env);
    client.propose_upgrade(
        &proposer,
        &symbol_short!("token"),
        &wasm_hash(&env, 2),
        &String::from_str(&env, "v2"),
        &0,
    );

    client.set_paused(&admin, &true);

    // Read entry points must stay available during an emergency stop.
    assert!(client.is_paused());
    assert!(client.get_proposal_query(&1u32).is_some());
    assert_eq!(client.get_votes_query(&1u32).len(), 0);
    assert_eq!(client.get_approved_wasm_hash(&symbol_short!("token")), None);
    assert_eq!(client.get_upgrade_history(&symbol_short!("token")).len(), 0);
}

#[test]
fn unpause_restores_mutations() {
    let env = Env::default();
    let (client, admin, _governor) = setup(&env);

    client.set_paused(&admin, &true);
    client.set_paused(&admin, &false);

    let proposer = Address::generate(&env);
    let proposal_id = client.propose_upgrade(
        &proposer,
        &symbol_short!("token"),
        &wasm_hash(&env, 2),
        &String::from_str(&env, "v2"),
        &0,
    );
    assert_eq!(proposal_id, 1);
}

#[test]
fn pause_flag_persists_across_storage_reload() {
    let env = Env::default();
    let (client, admin, _governor) = setup(&env);

    client.set_paused(&admin, &true);

    // Simulate a wasm upgrade at the same contract address: the guest re-reads
    // its (contract-persisted) instance storage. `is_paused` defaults the flag
    // to `false` when absent, so the pre-set `true` must survive the round-trip.
    let flag: bool = env.as_contract(&client.address, || {
        env.storage()
            .instance()
            .get::<DataKey, bool>(&DataKey::Paused)
            .expect("paused flag must be present")
    });
    assert!(flag);
    assert!(client.is_paused());
}

#[test]
fn upgrade_manifest_recorded_and_queryable() {
    let env = Env::default();
    let (client, _admin, _governor) = setup(&env);
    let proposer = Address::generate(&env);

    let proposal_id = client.propose_upgrade_with_manifest(
        &proposer,
        &symbol_short!("token"),
        &wasm_hash(&env, 5),
        &String::from_str(&env, "v2 schema migration"),
        &1000u64,
        &2u32,
        &1u32,
    );
    assert_eq!(proposal_id, 1);

    let manifest = client.get_upgrade_manifest(&1u32).expect("manifest found");
    assert_eq!(manifest.proposal_id, 1);
    assert_eq!(manifest.schema_version, 2);
    assert_eq!(manifest.min_compatible_schema, 1);
    assert_eq!(manifest.proposed_by, proposer);

    let manifests = client.get_contract_manifests(&symbol_short!("token"));
    assert_eq!(manifests.len(), 1);
    assert_eq!(manifests.get(0).unwrap().schema_version, 2);
}

#[test]
fn test_unauthorized_execution_fails() {
    let env = Env::default();
    let (client, _admin, governor) = setup(&env);
    let proposer = Address::generate(&env);
    let unauthorized = Address::generate(&env);

    let proposal_id = client.propose_upgrade(
        &proposer,
        &symbol_short!("token"),
        &wasm_hash(&env, 7),
        &String::from_str(&env, "auth test"),
        &100,
    );

    client.validate_upgrade(
        &governor,
        &proposal_id,
        &true,
        &String::from_str(&env, "valid"),
    );
    client.vote_upgrade(&governor, &proposal_id, &true);

    assert_eq!(
        client.try_execute_upgrade(&unauthorized, &proposal_id),
        Err(Ok(UpgradeError::NotGovernor))
    );
}

#[test]
fn test_premature_execution_fails() {
    let env = Env::default();
    let (client, _admin, governor) = setup(&env);
    let proposer = Address::generate(&env);

    let proposal_id = client.propose_upgrade(
        &proposer,
        &symbol_short!("token"),
        &wasm_hash(&env, 7),
        &String::from_str(&env, "timelock test"),
        &500, // 500 seconds delay
    );

    client.validate_upgrade(
        &governor,
        &proposal_id,
        &true,
        &String::from_str(&env, "valid"),
    );
    client.vote_upgrade(&governor, &proposal_id, &true);

    // Ledger timestamp is still at 0, timelock is 500
    assert_eq!(
        client.try_execute_upgrade(&governor, &proposal_id),
        Err(Ok(UpgradeError::TimelockNotElapsed))
    );
}

#[test]
fn test_altered_payload_fails() {
    let env = Env::default();
    let (client, _admin, governor) = setup(&env);
    let proposer = Address::generate(&env);
    let actual_hash = wasm_hash(&env, 7);
    let altered_hash = wasm_hash(&env, 99);

    let proposal_id = client.propose_upgrade(
        &proposer,
        &symbol_short!("token"),
        &actual_hash,
        &String::from_str(&env, "altered payload test"),
        &100,
    );

    client.validate_upgrade(
        &governor,
        &proposal_id,
        &true,
        &String::from_str(&env, "valid"),
    );
    client.vote_upgrade(&governor, &proposal_id, &true);

    env.ledger().with_mut(|l| l.timestamp += 101);

    assert_eq!(
        client.try_execute_upgrade_guarded(&governor, &proposal_id, &altered_hash),
        Err(Ok(UpgradeError::PayloadMismatch))
    );
}

#[test]
fn test_canceled_proposal_cannot_be_executed() {
    let env = Env::default();
    let (client, admin, governor) = setup(&env);
    let proposer = Address::generate(&env);

    let proposal_id = client.propose_upgrade(
        &proposer,
        &symbol_short!("token"),
        &wasm_hash(&env, 7),
        &String::from_str(&env, "cancel test"),
        &100,
    );

    client.validate_upgrade(
        &governor,
        &proposal_id,
        &true,
        &String::from_str(&env, "valid"),
    );
    client.vote_upgrade(&governor, &proposal_id, &true);

    // Cancel the proposal by proposer
    client.cancel_proposal(&proposer, &proposal_id, &symbol_short!("VULN_FOUND"));

    assert_eq!(
        client.get_proposal_status(&proposal_id),
        ProposalStatus::Canceled
    );

    env.ledger().with_mut(|l| l.timestamp += 101);

    assert_eq!(
        client.try_execute_upgrade(&governor, &proposal_id),
        Err(Ok(UpgradeError::ProposalCanceled))
    );
}

#[test]
fn test_expired_proposal_cannot_be_executed() {
    let env = Env::default();
    let (client, _admin, governor) = setup(&env);
    let proposer = Address::generate(&env);

    let proposal_id = client.propose_upgrade(
        &proposer,
        &symbol_short!("token"),
        &wasm_hash(&env, 7),
        &String::from_str(&env, "expiry test"),
        &100,
    );

    client.validate_upgrade(
        &governor,
        &proposal_id,
        &true,
        &String::from_str(&env, "valid"),
    );
    client.vote_upgrade(&governor, &proposal_id, &true);

    // Move timestamp past timelock + DEFAULT_GRACE_PERIOD (100 + 604_800 + 1)
    env.ledger()
        .with_mut(|l| l.timestamp += 100 + DEFAULT_GRACE_PERIOD + 1);

    assert_eq!(
        client.get_proposal_status(&proposal_id),
        ProposalStatus::Expired
    );

    assert_eq!(
        client.try_execute_upgrade(&governor, &proposal_id),
        Err(Ok(UpgradeError::ProposalExpired))
    );
}

#[test]
fn test_successful_upgrade_execution() {
    let env = Env::default();
    let (client, _admin, governor) = setup(&env);
    let proposer = Address::generate(&env);
    let expected_hash = wasm_hash(&env, 12);

    let proposal_id = client.propose_upgrade(
        &proposer,
        &symbol_short!("token"),
        &expected_hash,
        &String::from_str(&env, "successful upgrade"),
        &100,
    );

    assert_eq!(
        client.get_proposal_status(&proposal_id),
        ProposalStatus::Proposed
    );

    client.validate_upgrade(
        &governor,
        &proposal_id,
        &true,
        &String::from_str(&env, "audit passed"),
    );
    assert_eq!(
        client.get_proposal_status(&proposal_id),
        ProposalStatus::Validated
    );

    client.vote_upgrade(&governor, &proposal_id, &true);
    assert_eq!(
        client.get_proposal_status(&proposal_id),
        ProposalStatus::Approved
    );

    env.ledger().with_mut(|l| l.timestamp += 105);

    // Guarded execution succeeds
    client.execute_upgrade_guarded(&governor, &proposal_id, &expected_hash);

    assert_eq!(
        client.get_proposal_status(&proposal_id),
        ProposalStatus::Executed
    );
    assert_eq!(
        client.get_approved_wasm_hash(&symbol_short!("token")),
        Some(expected_hash.clone())
    );

    let history = client.get_upgrade_history(&symbol_short!("token"));
    assert_eq!(history.len(), 1);
    assert_eq!(history.get(0).unwrap().new_wasm_hash, expected_hash);
}

#[test]
fn test_emergency_pause_bounded_duration_prevents_permanent_lockout() {
    let env = Env::default();
    let (client, admin, _governor) = setup(&env);

    client.set_paused(&admin, &true);
    assert!(client.is_paused());

    // Advance ledger past MAX_PAUSE_DURATION (7 days)
    env.ledger()
        .with_mut(|l| l.timestamp += MAX_PAUSE_DURATION + 1);

    // Automatically expires!
    assert!(!client.is_paused());
}
