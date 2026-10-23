#![cfg(test)]

use super::{EscrowStatus, TournamentEscrow, TournamentEscrowClient};
use soroban_sdk::{testutils::Address as _, token, Address, BytesN, Env};

fn setup() -> (
    Env,
    TournamentEscrowClient<'static>,
    Address,
    Address,
    Address,
    Address,
    Address,
) {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let resolver = Address::generate(&env);
    let participant_a = Address::generate(&env);
    let participant_b = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let asset = env
        .register_stellar_asset_contract_v2(token_admin)
        .address();
    let contract_id = env.register(TournamentEscrow, ());
    let client = TournamentEscrowClient::new(&env, &contract_id);
    client.initialize(&admin, &resolver);
    let token_admin = token::StellarAssetClient::new(&env, &asset);
    token_admin.mint(&participant_a, &100);
    token_admin.mint(&participant_b, &100);
    (
        env,
        client,
        resolver,
        asset,
        participant_a,
        participant_b,
        contract_id,
    )
}

fn tournament_id(env: &Env) -> BytesN<32> {
    BytesN::from_array(env, &[7u8; 32])
}

#[test]
fn disputed_escrow_refunds_each_depositor_once() {
    let (env, client, resolver, asset, participant_a, participant_b, contract_id) = setup();
    let id = tournament_id(&env);
    client.create_escrow(&resolver, &id, &asset, &100);
    client.deposit(&id, &participant_a, &100);
    client.deposit(&id, &participant_b, &100);
    client.dispute(&resolver, &id);
    client.resolve(&resolver, &id, &None);

    let token = token::Client::new(&env, &asset);
    assert_eq!(token.balance(&participant_a), 100);
    assert_eq!(token.balance(&participant_b), 100);
    assert_eq!(token.balance(&contract_id), 0);
    let escrow = client.get_escrow(&id).unwrap();
    assert_eq!(escrow.status, EscrowStatus::Refunded);
    assert_eq!(escrow.total_held, 0);
}

#[test]
fn valid_result_releases_custody_and_marks_terminal_state() {
    let (env, client, resolver, asset, participant_a, participant_b, contract_id) = setup();
    let id = tournament_id(&env);
    client.create_escrow(&resolver, &id, &asset, &100);
    client.deposit(&id, &participant_a, &100);
    client.deposit(&id, &participant_b, &100);
    client.dispute(&resolver, &id);
    client.resolve(&resolver, &id, &Some(participant_a.clone()));

    let token = token::Client::new(&env, &asset);
    assert_eq!(token.balance(&participant_a), 200);
    assert_eq!(token.balance(&participant_b), 0);
    assert_eq!(token.balance(&contract_id), 0);
    let escrow = client.get_escrow(&id).unwrap();
    assert_eq!(escrow.status, EscrowStatus::Released);
    assert_eq!(escrow.total_held, 0);
    assert!(client
        .try_resolve(&resolver, &id, &Some(participant_a.clone()))
        .is_err());
}

#[test]
fn failed_deposit_transfer_does_not_change_escrow_accounting() {
    let (env, client, resolver, asset, _, _, contract_id) = setup();
    let id = tournament_id(&env);
    let unfunded = Address::generate(&env);
    client.create_escrow(&resolver, &id, &asset, &100);

    assert!(client.try_deposit(&id, &unfunded, &100).is_err());
    let escrow = client.get_escrow(&id).unwrap();
    assert_eq!(escrow.total_held, 0);
    assert_eq!(escrow.participants.len(), 0);
    assert_eq!(token::Client::new(&env, &asset).balance(&contract_id), 0);
}

#[test]
fn resolver_can_cancel_an_unfunded_tournament() {
    let (env, client, resolver, asset, _, _, _) = setup();
    let id = tournament_id(&env);
    client.create_escrow(&resolver, &id, &asset, &100);
    client.resolve(&resolver, &id, &None);

    let escrow = client.get_escrow(&id).unwrap();
    assert_eq!(escrow.status, EscrowStatus::Refunded);
    assert_eq!(escrow.total_held, 0);
}

#[test]
fn test_pause_boundaries_block_risk_operations() {
    let (env, client, resolver, asset, participant_a, _, _) = setup();
    let id = tournament_id(&env);
    let admin = client.get_admin();
    let guardian = Address::generate(&env);

    client.set_guardian(&admin, &guardian);
    client.create_escrow(&resolver, &id, &asset, &100);

    // Guardian triggers emergency pause for 3600 seconds
    client.emergency_pause(&guardian, &soroban_sdk::symbol_short!("EXPLOIT"), &3600);
    assert!(client.is_emergency_paused());

    // Deposit is blocked while paused
    assert!(client.try_deposit(&id, &participant_a, &100).is_err());

    // Creating another escrow is blocked while paused
    let id2 = BytesN::from_array(&env, &[8u8; 32]);
    assert!(client
        .try_create_escrow(&resolver, &id2, &asset, &100)
        .is_err());
}

#[test]
fn test_pause_expiry_avoids_permanent_lockout() {
    let (env, client, resolver, asset, participant_a, _, _) = setup();
    let id = tournament_id(&env);
    let admin = client.get_admin();
    let guardian = Address::generate(&env);

    client.set_guardian(&admin, &guardian);
    client.create_escrow(&resolver, &id, &asset, &100);

    client.emergency_pause(&guardian, &soroban_sdk::symbol_short!("ALERT"), &1000);
    assert!(client.is_emergency_paused());

    // Advance ledger past expiry window (1000 + 1)
    env.ledger().with_mut(|l| l.timestamp += 1001);

    // Pause expires automatically without admin intervention
    assert!(!client.is_emergency_paused());

    // Deposit succeeds again
    client.deposit(&id, &participant_a, &100);
    let escrow = client.get_escrow(&id).unwrap();
    assert_eq!(escrow.total_held, 100);
}

#[test]
fn test_authorized_resume_by_admin() {
    let (env, client, _resolver, _asset, _, _, _) = setup();
    let admin = client.get_admin();
    let intruder = Address::generate(&env);

    client.emergency_pause(&admin, &soroban_sdk::symbol_short!("MAINT"), &5000);
    assert!(client.is_emergency_paused());

    // Intruder cannot unpause
    assert!(client.try_unpause(&intruder).is_err());
    assert!(client.is_emergency_paused());

    // Admin unpauses
    client.unpause(&admin);
    assert!(!client.is_emergency_paused());
}

#[test]
fn test_user_fund_exit_behavior_during_emergency_pause() {
    let (env, client, resolver, asset, participant_a, participant_b, contract_id) = setup();
    let id = tournament_id(&env);
    let admin = client.get_admin();
    let guardian = Address::generate(&env);

    client.set_guardian(&admin, &guardian);
    client.create_escrow(&resolver, &id, &asset, &100);
    client.deposit(&id, &participant_a, &100);
    client.deposit(&id, &participant_b, &100);

    // Exploit or oracle failure triggers emergency pause
    client.emergency_pause(
        &guardian,
        &soroban_sdk::symbol_short!("ORACLE_ERR"),
        &86_400,
    );
    assert!(client.is_emergency_paused());

    // Winner resolution is blocked while paused
    assert!(client
        .try_resolve(&resolver, &id, &Some(participant_a.clone()))
        .is_err());

    // Users are NOT permanently locked out: participant_a exits funds safely
    client.emergency_withdraw(&id, &participant_a);

    let token = token::Client::new(&env, &asset);
    assert_eq!(token.balance(&participant_a), 100);
    assert_eq!(token.balance(&contract_id), 100);

    let escrow = client.get_escrow(&id).unwrap();
    assert_eq!(escrow.total_held, 100);
    assert!(!escrow.deposits.contains_key(participant_a.clone()));

    // Participant B also exits safely
    client.emergency_withdraw(&id, &participant_b);
    assert_eq!(token.balance(&participant_b), 100);
    assert_eq!(token.balance(&contract_id), 0);

    let escrow_final = client.get_escrow(&id).unwrap();
    assert_eq!(escrow_final.total_held, 0);
}
