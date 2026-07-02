use super::*;

fn test_config() -> AppConfig {
    AppConfig {
        upstream_concurrency_retry_attempts: 4,
        upstream_concurrency_retry_backoff_ms: 50,
        upstream_concurrency_retry_max_wait_seconds: 10,
        upstream_concurrency_retry_exclusive_wait_multiplier: 2,
        ..AppConfig::default()
    }
}

#[test]
fn shared_budget_uses_the_base_window() {
    let config = test_config();

    assert_eq!(concurrency_retry_budget_ms(&config, false), 10_000);
}

#[test]
fn exclusive_budget_scales_the_wait_window() {
    let config = test_config();

    assert_eq!(concurrency_retry_budget_ms(&config, true), 20_000);
}

#[test]
fn base_delay_grows_exponentially() {
    let config = test_config();

    assert_eq!(concurrency_retry_base_delay_ms(&config, 0), 50);
    assert_eq!(concurrency_retry_base_delay_ms(&config, 1), 100);
    assert_eq!(concurrency_retry_base_delay_ms(&config, 2), 200);
}

#[test]
fn jitter_is_deterministic_for_the_same_seed() {
    let jitter_a = deterministic_jitter_ms(25, "req-1", "up-1", "gpt-4.1-mini", 3);
    let jitter_b = deterministic_jitter_ms(25, "req-1", "up-1", "gpt-4.1-mini", 3);

    assert_eq!(jitter_a, jitter_b);
    assert!(jitter_a <= 25);
}

#[test]
fn retry_budget_extends_for_exclusive_models() {
    let config = AppConfig {
        upstream_concurrency_retry_attempts: 4,
        upstream_concurrency_retry_backoff_ms: 50,
        upstream_concurrency_retry_max_wait_seconds: 1,
        upstream_concurrency_retry_exclusive_wait_multiplier: 2,
        ..AppConfig::default()
    };

    assert!(
        plan_concurrency_retry(&config, 0, 1_500, false, "req-1", "up-1", "gpt-4.1-mini").is_none()
    );

    let plan = plan_concurrency_retry(&config, 0, 1_500, true, "req-1", "up-1", "gpt-4.1-mini")
        .expect("exclusive model should still have retry budget");
    assert!(plan.sleep_ms > 0);
}

#[test]
fn retry_plan_caps_to_remaining_budget() {
    let config = AppConfig {
        upstream_concurrency_retry_attempts: 4,
        upstream_concurrency_retry_backoff_ms: 50,
        upstream_concurrency_retry_max_wait_seconds: 10,
        upstream_concurrency_retry_exclusive_wait_multiplier: 2,
        ..AppConfig::default()
    };

    let plan = plan_concurrency_retry(&config, 0, 9_990, false, "req-1", "up-1", "gpt-4.1-mini")
        .expect("remaining budget should allow one short retry");

    assert_eq!(plan.sleep_ms, 10);
    assert_eq!(plan.retry_after_seconds, 1);
}
