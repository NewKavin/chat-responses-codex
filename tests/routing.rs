use chat_responses_codex::routing::{
    select_upstream, RouteError, RouteRequest, UpstreamCandidate, UpstreamProtocol,
};
use chat_responses_codex::state::{AppConfig, AppState, PersistedState, UpstreamConfig};
use std::sync::Arc;

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

#[tokio::test]
async fn app_state_routing_honors_model_case_matching_switch() {
    let upstream = UpstreamConfig {
        id: "upper".into(),
        name: "upper".into(),
        protocol: UpstreamProtocol::ChatCompletions,
        protocols: vec![UpstreamProtocol::ChatCompletions],
        supported_models: vec!["GLM-4.5".into()],
        active: true,
        ..UpstreamConfig::default()
    };

    for (case_insensitive, should_match) in [(true, true), (false, false)] {
        let state = AppState::new(
            PersistedState {
                upstreams: Arc::new(vec![upstream.clone()]),
                ..PersistedState::default()
            },
            tempfile::tempdir().unwrap().path().join("state.json"),
            AppConfig {
                model_case_insensitive_matching: case_insensitive,
                ..AppConfig::default()
            },
        );

        assert_eq!(
            state
                .choose_upstream("glm-4.5", UpstreamProtocol::ChatCompletions)
                .await
                .is_ok(),
            should_match
        );
    }
}

#[test]
fn affinity_keys_honor_model_case_matching_switch() {
    for (case_insensitive, expected) in [(true, Some("upper")), (false, None)] {
        let state = AppState::new(
            PersistedState::default(),
            tempfile::tempdir().unwrap().path().join("state.json"),
            AppConfig {
                model_case_insensitive_matching: case_insensitive,
                ..AppConfig::default()
            },
        );
        state.set_affinity_upstream("down", "GLM-4.5", "upper", 60);

        assert_eq!(
            state.get_affinity_upstream("down", "glm-4.5").as_deref(),
            expected
        );
    }
}
