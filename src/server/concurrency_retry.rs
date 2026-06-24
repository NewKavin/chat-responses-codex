use crate::state::AppConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ConcurrencyRetryPlan {
    pub sleep_ms: u64,
    pub retry_after_seconds: u64,
}

pub(crate) fn is_exclusive_model(candidate_count: usize) -> bool {
    candidate_count <= 1
}

pub(crate) fn concurrency_retry_budget_ms(config: &AppConfig, exclusive_model: bool) -> u64 {
    let base_budget_ms = config
        .upstream_concurrency_retry_max_wait_seconds
        .max(1)
        .saturating_mul(1000);
    let multiplier = if exclusive_model {
        config
            .upstream_concurrency_retry_exclusive_wait_multiplier
            .max(1)
    } else {
        1
    };

    base_budget_ms.saturating_mul(multiplier)
}

pub(crate) fn concurrency_retry_base_delay_ms(config: &AppConfig, attempts_used: u32) -> u64 {
    let base_delay_ms = config.upstream_concurrency_retry_backoff_ms.max(1);
    let exponent = attempts_used.min(20);
    let factor = 1u64.checked_shl(exponent).unwrap_or(u64::MAX);
    base_delay_ms.saturating_mul(factor).max(1)
}

fn fnv1a64(mut hash: u64, bytes: &[u8]) -> u64 {
    const PRIME: u64 = 0x100000001b3;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

fn deterministic_seed(request_id: &str, upstream_id: &str, model: &str, attempts_used: u32) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    let mut hash = OFFSET_BASIS;
    hash = fnv1a64(hash, request_id.as_bytes());
    hash = fnv1a64(hash, &[0xff]);
    hash = fnv1a64(hash, upstream_id.as_bytes());
    hash = fnv1a64(hash, &[0xfe]);
    hash = fnv1a64(hash, model.as_bytes());
    hash = fnv1a64(hash, &[0xfd]);
    fnv1a64(hash, &attempts_used.to_le_bytes())
}

pub(crate) fn deterministic_jitter_ms(
    max_jitter_ms: u64,
    request_id: &str,
    upstream_id: &str,
    model: &str,
    attempts_used: u32,
) -> u64 {
    if max_jitter_ms == 0 {
        return 0;
    }

    deterministic_seed(request_id, upstream_id, model, attempts_used) % (max_jitter_ms + 1)
}

pub(crate) fn plan_concurrency_retry(
    config: &AppConfig,
    attempts_used: u32,
    elapsed_ms: u64,
    exclusive_model: bool,
    request_id: &str,
    upstream_id: &str,
    model: &str,
) -> Option<ConcurrencyRetryPlan> {
    if attempts_used >= config.upstream_concurrency_retry_attempts.max(1) {
        return None;
    }

    let budget_ms = concurrency_retry_budget_ms(config, exclusive_model);
    if elapsed_ms >= budget_ms {
        return None;
    }

    let remaining_ms = budget_ms - elapsed_ms;
    let raw_delay_ms = concurrency_retry_base_delay_ms(config, attempts_used);
    let jitter_limit_ms = (raw_delay_ms / 4).clamp(1, 250);
    let jitter_ms = deterministic_jitter_ms(
        jitter_limit_ms,
        request_id,
        upstream_id,
        model,
        attempts_used,
    );
    let sleep_ms = raw_delay_ms
        .saturating_add(jitter_ms)
        .min(remaining_ms)
        .max(1);

    Some(ConcurrencyRetryPlan {
        sleep_ms,
        retry_after_seconds: sleep_ms.saturating_add(999) / 1000,
    })
}

#[cfg(test)]
mod tests {
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
            plan_concurrency_retry(&config, 0, 1_500, false, "req-1", "up-1", "gpt-4.1-mini")
                .is_none()
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

        let plan =
            plan_concurrency_retry(&config, 0, 9_990, false, "req-1", "up-1", "gpt-4.1-mini")
                .expect("remaining budget should allow one short retry");

        assert_eq!(plan.sleep_ms, 10);
        assert_eq!(plan.retry_after_seconds, 1);
    }
}
