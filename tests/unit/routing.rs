use chat_responses_codex::routing::{
    select_upstream, RouteRequest, UpstreamCandidate, UpstreamProtocol,
};

#[test]
fn test_priority_based_selection_picks_highest_priority_healthy_upstream() {
    // 说明：premium 配额保护路由（“非 premium 模型避开 premium 账号”）已随
    // 446385a3 下线，当前 select_upstream 按优先级+健康度选择；这里保留对
    // “最高优先级且支持该模型”行为的覆盖。
    let premium_account = UpstreamCandidate::new(
        "premium",
        "Premium Account",
        UpstreamProtocol::ChatCompletions,
    )
    .with_models(vec!["gpt-4", "gpt-3.5-turbo", "glm-5.1"])
    .with_priority(100);

    let regular_account = UpstreamCandidate::new(
        "regular",
        "Regular Account",
        UpstreamProtocol::ChatCompletions,
    )
    .with_models(vec!["gpt-4", "gpt-3.5-turbo"])
    .with_priority(50);

    let request = RouteRequest::new("gpt-4", UpstreamProtocol::ChatCompletions, false);
    let result = select_upstream(
        &request,
        &[premium_account.clone(), regular_account.clone()],
    );

    assert!(result.is_ok());
    assert_eq!(result.unwrap().id, "premium");

    let request_glm = RouteRequest::new("glm-5.1", UpstreamProtocol::ChatCompletions, false);
    let result_glm = select_upstream(
        &request_glm,
        &[premium_account.clone(), regular_account.clone()],
    );

    // glm-5.1 只有 premium 账号支持，仍选 premium
    assert!(result_glm.is_ok());
    assert_eq!(result_glm.unwrap().id, "premium");
}

#[test]
fn test_fallback_to_premium_when_no_other_option() {
    let premium_account = UpstreamCandidate::new(
        "premium",
        "Premium Account",
        UpstreamProtocol::ChatCompletions,
    )
    .with_models(vec!["gpt-4", "glm-5.1"])
    .with_priority(100);

    let request = RouteRequest::new("gpt-4", UpstreamProtocol::ChatCompletions, false);
    let result = select_upstream(&request, std::slice::from_ref(&premium_account));

    // Should fall back to premium account when it's the only option
    assert!(result.is_ok());
    assert_eq!(result.unwrap().id, "premium");
}
