use chat_responses_codex::routing::{
    select_upstream, RouteRequest, UpstreamCandidate, UpstreamProtocol,
};

#[test]
fn test_avoid_premium_account_for_non_premium_model() {
    let premium_account = UpstreamCandidate::new(
        "premium",
        "Premium Account",
        UpstreamProtocol::ChatCompletions,
    )
    .with_models(vec!["gpt-4", "gpt-3.5-turbo", "glm-5.1"])
    .with_premium_models(vec!["glm-5.1"])
    .with_protect_premium_quota(true)
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

    // Should select regular account even though premium has higher priority
    assert!(result.is_ok());
    assert_eq!(result.unwrap().id, "regular");
}

#[test]
fn test_use_premium_account_for_premium_model() {
    let premium_account = UpstreamCandidate::new(
        "premium",
        "Premium Account",
        UpstreamProtocol::ChatCompletions,
    )
    .with_models(vec!["gpt-4", "glm-5.1"])
    .with_premium_models(vec!["glm-5.1"])
    .with_protect_premium_quota(true)
    .with_priority(100);

    let regular_account = UpstreamCandidate::new(
        "regular",
        "Regular Account",
        UpstreamProtocol::ChatCompletions,
    )
    .with_models(vec!["gpt-4"])
    .with_priority(50);

    let request = RouteRequest::new("glm-5.1", UpstreamProtocol::ChatCompletions, false);
    let result = select_upstream(
        &request,
        &[premium_account.clone(), regular_account.clone()],
    );

    // Should select premium account for premium model
    assert!(result.is_ok());
    assert_eq!(result.unwrap().id, "premium");
}

#[test]
fn test_fallback_to_premium_when_no_other_option() {
    let premium_account = UpstreamCandidate::new(
        "premium",
        "Premium Account",
        UpstreamProtocol::ChatCompletions,
    )
    .with_models(vec!["gpt-4", "glm-5.1"])
    .with_premium_models(vec!["glm-5.1"])
    .with_protect_premium_quota(true)
    .with_priority(100);

    let request = RouteRequest::new("gpt-4", UpstreamProtocol::ChatCompletions, false);
    let result = select_upstream(&request, &[premium_account.clone()]);

    // Should fall back to premium account when it's the only option
    assert!(result.is_ok());
    assert_eq!(result.unwrap().id, "premium");
}
