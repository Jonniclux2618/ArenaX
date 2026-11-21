//! Rust enforcement checks against the TypeScript/JavaScript shared policy fixture.

use serde::Deserialize;
use std::collections::BTreeSet;
use std::fs;

#[derive(Debug, Deserialize)]
struct Policy {
    operations: Vec<Row>,
}

#[derive(Debug, Deserialize)]
struct Row {
    operation: String,
    principal: String,
    resource: String,
    action: String,
    permission: String,
    owner: String,
    decision: String,
}

fn fixture() -> Policy {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../server/test/fixtures/authorization-policy.json"
    );
    let text = fs::read_to_string(path).expect("shared authorization fixture must exist");
    serde_json::from_str(&text).expect("shared authorization fixture must be valid JSON")
}

fn role_permissions(role: &str) -> BTreeSet<&'static str> {
    match role {
        "ADMIN" => [
            "users:read", "users:write", "users:delete", "games:read", "games:write",
            "matches:read", "matches:write", "tournaments:read", "tournaments:write",
            "tournaments:admin", "analytics:read", "analytics:write", "analytics:admin",
            "staking:read", "staking:write", "staking:admin", "assets:read", "assets:write",
            "assets:bridge", "system:read", "system:write", "moderation:read",
            "moderation:write", "payments:read", "payments:write",
        ].into_iter().collect(),
        "OPERATOR" => [
            "users:read", "users:write", "games:read", "games:write", "matches:read",
            "matches:write", "tournaments:read", "tournaments:write", "tournaments:admin",
            "analytics:read", "analytics:write", "analytics:admin", "staking:read",
            "staking:write", "staking:admin", "assets:read", "assets:write", "assets:bridge",
            "moderation:read", "moderation:write", "payments:read", "payments:write",
        ].into_iter().collect(),
        "MODERATOR" => ["moderation:read", "moderation:write", "users:read", "games:read"].into_iter().collect(),
        "TOURNAMENT_ORGANIZER" => ["tournaments:read", "tournaments:write", "tournaments:admin", "analytics:read"].into_iter().collect(),
        "USER" => ["games:read", "matches:read", "tournaments:read"].into_iter().collect(),
        _ => BTreeSet::new(),
    }
}

#[test]
fn shared_fixture_matches_rust_decisions() {
    let policy = fixture();
    assert!(!policy.operations.is_empty());
    for row in policy.operations {
        let has_permission = role_permissions(&row.principal).contains(row.permission.as_str());
        let allowed = row.decision == "allow" && has_permission;
        assert_eq!(
            allowed,
            row.decision == "allow",
            "{}:{}:{}",
            row.operation,
            row.resource,
            row.action
        );
    }
}
