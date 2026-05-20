use chat_responses_codex::routing::{
    select_upstream, RouteError, RouteRequest, UpstreamCandidate, UpstreamProtocol,
};

#[test]
fn selects_first_healthy_supported_upstream_and_falls_back() {
    let request = RouteRequest::new("gpt-4.1-mini", UpstreamProtocol::ChatCompletions, false);
    let candidates = vec![
        UpstreamCandidate::new("a", "primary", UpstreamProtocol::ChatCompletions)
            .with_models(["gpt-4.1-mini"])
            .with_failure_count(3),
        UpstreamCandidate::new("b", "backup", UpstreamProtocol::ChatCompletions)
            .with_models(["gpt-4.1-mini"]),
    ];

    let selected = select_upstream(&request, &candidates).expect("an upstream should be selected");

    assert_eq!(selected.id, "b");
}

#[test]
fn rejects_when_no_upstream_supports_requested_model() {
    let request = RouteRequest::new("gpt-4.1-mini", UpstreamProtocol::ChatCompletions, false);
    let candidates =
        vec![
            UpstreamCandidate::new("a", "primary", UpstreamProtocol::ChatCompletions)
                .with_models(["gpt-4o-mini"]),
        ];

    let err = select_upstream(&request, &candidates).unwrap_err();

    assert_eq!(
        err,
        RouteError::ModelUnavailable("gpt-4.1-mini".to_string())
    );
}
