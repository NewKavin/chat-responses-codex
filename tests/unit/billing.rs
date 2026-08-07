use chat_responses_codex::state::DownstreamConfig;

fn cost_downstream() -> DownstreamConfig {
    DownstreamConfig {
        id: "cost-billing-unit".into(),
        name: "Cost billing unit".into(),
        hash: "hash".into(),
        plaintext_key: None,
        plaintext_key_prefix: None,
        model_allowlist: Vec::new(),
        rate_limit_enabled: true,
        per_minute_limit: 60,
        max_concurrency: 10,
        daily_token_limit: None,
        monthly_token_limit: None,
        input_token_price_per_million_cents: None,
        output_token_price_per_million_cents: None,
        daily_cost_limit_cents: None,
        request_quota_window_hours: None,
        request_quota_requests: None,
        ip_allowlist: Vec::new(),
        expires_at: None,
        active: true,
        billing_mode: "token".into(),
    }
}

#[test]
fn cost_for_tokens_prices_input_and_output_independently() {
    let mut downstream = cost_downstream();
    downstream.input_token_price_per_million_cents = Some(1_000); // 10 元 / 1M 输入
    downstream.output_token_price_per_million_cents = Some(3_000); // 30 元 / 1M 输出

    // 1M input + 1M output = 1000c + 3000c = 4000c = 40 元
    assert_eq!(downstream.cost_for_tokens(1_000_000, 1_000_000), 4_000);
    // 500k input only = 500c
    assert_eq!(downstream.cost_for_tokens(500_000, 0), 500);
    // 100k output only = 300c
    assert_eq!(downstream.cost_for_tokens(0, 100_000), 300);
    // 1234 input + 5678 output（不足 1M 的部分按比例四舍五入取整）
    assert_eq!(downstream.cost_for_tokens(1_234, 5_678), 1 + 17);
}

#[test]
fn cost_for_tokens_missing_price_contributes_zero() {
    let mut downstream = cost_downstream();
    downstream.input_token_price_per_million_cents = Some(2_000);

    assert_eq!(downstream.cost_for_tokens(1_000_000, 1_000_000), 2_000);
    assert_eq!(downstream.cost_for_tokens(0, 5_000_000), 0);
}

#[test]
fn cost_for_tokens_without_any_price_is_zero() {
    let downstream = cost_downstream();
    assert_eq!(downstream.cost_for_tokens(1_000_000, 1_000_000), 0);
}

#[test]
fn cost_billing_mode_requires_token_mode_price_and_cost_limit() {
    let base = cost_downstream();

    let mut token_only = base.clone();
    token_only.billing_mode = "token".into();
    assert!(!token_only.cost_billing_mode());

    let mut priced = base.clone();
    priced.billing_mode = "token".into();
    priced.input_token_price_per_million_cents = Some(1_000);
    assert!(
        !priced.cost_billing_mode(),
        "price without cost limit must not enable cost billing"
    );

    let mut limited = priced.clone();
    limited.daily_cost_limit_cents = Some(3_000);
    assert!(limited.cost_billing_mode());

    let mut output_only = base.clone();
    output_only.billing_mode = "token".into();
    output_only.output_token_price_per_million_cents = Some(2_000);
    output_only.daily_cost_limit_cents = Some(3_000);
    assert!(
        output_only.cost_billing_mode(),
        "output-only price must enable cost billing"
    );

    let mut request_mode = limited.clone();
    request_mode.billing_mode = "request".into();
    assert!(!request_mode.cost_billing_mode());
}
