use super::*;
use std::collections::HashMap;

fn owners(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(key, user)| (key.to_string(), user.to_string()))
        .collect()
}

fn limits(pairs: &[(&str, u64)]) -> HashMap<String, u64> {
    pairs
        .iter()
        .map(|(scope, limit)| (scope.to_string(), *limit))
        .collect()
}

#[test]
fn owned_key_resolves_to_its_user_scope_and_limit() {
    let scope = resolve_cost_scope(
        "key-a",
        &owners(&[("key-a", "user-1")]),
        &limits(&[("user-1", 5_000)]),
    );
    assert_eq!(scope.scope_id, "user-1");
    assert_eq!(scope.daily_limit_cents, Some(5_000));
}

#[test]
fn sibling_keys_of_one_user_share_one_scope() {
    let owners = owners(&[("key-a", "user-1"), ("key-b", "user-1")]);
    let limits = limits(&[("user-1", 5_000)]);
    let first = resolve_cost_scope("key-a", &owners, &limits);
    let second = resolve_cost_scope("key-b", &owners, &limits);
    assert_eq!(first.scope_id, second.scope_id);
    assert_eq!(first.daily_limit_cents, second.daily_limit_cents);
}

#[test]
fn unowned_key_is_its_own_scope() {
    let scope = resolve_cost_scope(
        "key-direct",
        &HashMap::new(),
        &limits(&[("key-direct", 800)]),
    );
    assert_eq!(scope.scope_id, "key-direct");
    assert_eq!(scope.daily_limit_cents, Some(800));
}

#[test]
fn scope_without_a_configured_limit_is_unlimited() {
    let scope = resolve_cost_scope(
        "key-a",
        &owners(&[("key-a", "user-1")]),
        &HashMap::new(),
    );
    assert_eq!(scope.scope_id, "user-1");
    assert_eq!(scope.daily_limit_cents, None);
}

#[test]
fn a_key_level_limit_never_applies_to_an_owned_key() {
    // 账号有归属时，以 Key id 为 scope_id 写的上限必须完全不生效，
    // 否则切换后会留下 Key 级残留限制。
    let scope = resolve_cost_scope(
        "key-a",
        &owners(&[("key-a", "user-1")]),
        &limits(&[("key-a", 1), ("user-1", 5_000)]),
    );
    assert_eq!(scope.scope_id, "user-1");
    assert_eq!(scope.daily_limit_cents, Some(5_000));
}
