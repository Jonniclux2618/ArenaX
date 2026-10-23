#![cfg(test)]

use auth_gateway::{AuthGateway, AuthGatewayClient, Role};
use composable_example::{ComposableExample, ComposableExampleClient};
use soroban_sdk::{testutils::Address as _, Address, Env};

#[test]
fn test_composable_contract() {
    let env = Env::default();
    env.mock_all_auths();

    let owner = Address::generate(&env);
    let contract_id = env.register(ComposableExample, ());
    let client = ComposableExampleClient::new(&env, &contract_id);

    // Initialize
    client.initialize(&owner);

    // Check initial state
    assert_eq!(client.owner(), owner);
    assert_eq!(client.get_counter(), 0);
    assert!(!client.is_paused());

    // Test increment/decrement
    assert_eq!(client.increment(), 1);
    assert_eq!(client.increment(), 2);
    assert_eq!(client.decrement(), 1);
    assert_eq!(client.get_counter(), 1);

    // Test pause
    client.set_paused(&true);
    assert!(client.is_paused());

    // Test pause blocks operations
    let result = client.try_increment();
    assert!(result.is_err());

    // Test unpause
    client.set_paused(&false);
    assert!(!client.is_paused());

    // Test transfer ownership
    let new_owner = Address::generate(&env);
    client.transfer_ownership(&new_owner);
    assert_eq!(client.owner(), new_owner);
}

// Test composition with access-control
#[test]
fn test_contract_composition_with_access_control() {
    let env = Env::default();
    env.mock_all_auths();

    // Deploy access-control contract
    let access_admin = Address::generate(&env);
    let access_id = env.register(access_control::AccessControl, ());
    let access_client = access_control::AccessControlClient::new(&env, &access_id);
    access_client.initialize(&access_admin);

    // Grant operator role to test user
    let operator = Address::generate(&env);
    access_client.grant_role(&operator, &3); // ROLE_OPERATOR

    // Deploy composable example contract
    let owner = Address::generate(&env);
    let example_id = env.register(ComposableExample, ());
    let example_client = ComposableExampleClient::new(&env, &example_id);
    example_client.initialize(&owner);

    // Verify composition works (both contracts deployed independently)
    assert!(access_client.has_role(&operator, &3));
    assert_eq!(example_client.get_counter(), 0);
}

#[test]
fn test_role_administration_is_consistent_across_contracts() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let next_admin = Address::generate(&env);
    let operator = Address::generate(&env);
    let access_id = env.register(access_control::AccessControl, ());
    let access = access_control::AccessControlClient::new(&env, &access_id);
    let gateway_id = env.register(AuthGateway, ());
    let gateway = AuthGatewayClient::new(&env, &gateway_id);

    access.initialize(&admin);
    gateway.initialize(&admin);

    access.grant_role(&operator, &access_control::ROLE_OPERATOR);
    gateway.assign_role(&operator, &Role::Operator);
    assert!(access
        .try_grant_role(&access_id, &access_control::ROLE_OPERATOR)
        .is_err());
    assert!(gateway
        .try_assign_role(&gateway_id, &Role::Operator)
        .is_err());
    assert!(access.has_role(&operator, &access_control::ROLE_OPERATOR));
    assert!(gateway.has_role(&operator, &Role::Operator));
    assert!(access
        .try_grant_role(&operator, &access_control::ROLE_ADMIN)
        .is_err());
    assert!(gateway.try_assign_role(&operator, &Role::Admin).is_err());

    access.revoke_role(&operator, &access_control::ROLE_OPERATOR);
    gateway.revoke_role(&operator);
    assert!(!access.has_role(&operator, &access_control::ROLE_OPERATOR));
    assert!(!gateway.has_role(&operator, &Role::Operator));

    access.transfer_admin(&next_admin);
    gateway.transfer_admin(&next_admin);
    assert_eq!(access.get_admin(), next_admin);
    assert_eq!(gateway.get_admin(), next_admin);
    assert!(!access.has_role(&admin, &access_control::ROLE_ADMIN));
    assert!(!gateway.has_role(&admin, &Role::Admin));
    assert!(access
        .try_revoke_role(&next_admin, &access_control::ROLE_ADMIN)
        .is_err());
    assert!(gateway.try_revoke_role(&next_admin).is_err());

    env.mock_auths(&[]);
    assert!(access
        .try_grant_role(&operator, &access_control::ROLE_OPERATOR)
        .is_err());
    assert!(gateway.try_assign_role(&operator, &Role::Operator).is_err());
}
