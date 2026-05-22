use gateway_core::admin::{
    normalize_fetched_models, DownstreamFormView, DownstreamListQuery, UpstreamFormView,
};
use gateway_core::routing::UpstreamProtocol;
use gateway_core::state::{
    AppConfig, DownstreamConfig, ModelAliasConfig, ModelRequestCostConfig, PersistedState,
    UpstreamConfig, UsageLog,
};

pub(crate) fn login_config() -> AppConfig {
    AppConfig::default()
}

pub(crate) fn dashboard_context() -> (AppConfig, PersistedState) {
    (login_config(), sample_state())
}

pub(crate) fn upstreams_context(
    edit_id: Option<&str>,
) -> (AppConfig, PersistedState, UpstreamFormView, String, bool) {
    let state = sample_state();
    let (models, aliases) = normalize_fetched_models(vec![
        "GLM-5".into(),
        "glm-5.1".into(),
        "GPT-4.1-MINI".into(),
    ]);
    let selected = edit_id.and_then(|id| state.upstreams.iter().find(|upstream| upstream.id == id));
    let form = selected
        .map(UpstreamFormView::from_upstream)
        .unwrap_or_else(UpstreamFormView::blank);
    let form_open = selected.is_some();

    let notice = if form_open {
        format!(
            "当前正在编辑上游 {}，表单会自动展开并带入已有配置。当前别名示例：{models} / {aliases}。",
            selected.expect("selected is_some above").name
        )
    } else {
        format!(
            "点击右上角的新增按钮会展开上游表单；保存前可以先用“获取当前模型”把上游抓到的模型和别名自动填进来。这里只会为真正需要保留原始大小写的模型生成别名，已经是小写的项不会额外写成自映射。当前别名示例：{models} / {aliases}。"
        )
    };

    (login_config(), state, form, notice, form_open)
}

pub(crate) fn downstreams_context(
    edit_id: Option<&str>,
    query: DownstreamListQuery,
) -> (
    AppConfig,
    PersistedState,
    DownstreamFormView,
    DownstreamListQuery,
    String,
    bool,
) {
    let state = sample_state();
    let selected = edit_id.and_then(|id| {
        state
            .downstreams
            .iter()
            .find(|downstream| downstream.id == id)
    });
    let form = selected
        .map(DownstreamFormView::from_downstream)
        .unwrap_or_else(DownstreamFormView::blank);
    let form_open = selected.is_some();
    let notice = if form_open {
        format!(
            "当前正在编辑下游 {}，表单会自动展开并带入已有配置。点击列表里的编辑会保留当前筛选条件。",
            selected.expect("selected is_some above").name
        )
    } else {
        "点击右上角的新增按钮会展开下游密钥表单，列表仍然来自共享 state。".to_string()
    };

    (login_config(), state, form, query, notice, form_open)
}

pub(crate) fn logs_context() -> (AppConfig, PersistedState) {
    (login_config(), sample_state())
}

pub(crate) fn portal_context() -> (AppConfig, PersistedState) {
    (login_config(), sample_state())
}

fn sample_state() -> PersistedState {
    PersistedState {
        upstreams: sample_upstreams(),
        downstreams: sample_downstreams(),
        usage_logs: sample_usage_logs(),
    }
}

fn sample_upstreams() -> Vec<UpstreamConfig> {
    vec![
        UpstreamConfig {
            id: "up-glm-primary".into(),
            name: "GLM 主账号".into(),
            base_url: "https://open.bigmodel.cn/api/paas/v4".into(),
            api_key: "sk-demo-glm-primary".into(),
            protocol: UpstreamProtocol::Responses,
            supported_models: vec!["glm-5".into(), "glm-5.1".into()],
            model_aliases: vec![
                ModelAliasConfig {
                    slug: "glm-5".into(),
                    upstream_model: "GLM-5".into(),
                },
                ModelAliasConfig {
                    slug: "glm-5.1".into(),
                    upstream_model: "GLM-5.1".into(),
                },
            ],
            request_quota_5h: 600,
            requests_per_minute: 20,
            max_concurrency: 4,
            model_request_costs: vec![
                ModelRequestCostConfig {
                    slug: "glm-5".into(),
                    cost: 2,
                },
                ModelRequestCostConfig {
                    slug: "glm-5.1".into(),
                    cost: 2,
                },
            ],
            active: true,
            failure_count: 0,
        },
        UpstreamConfig {
            id: "up-openai-compat".into(),
            name: "OpenAI 兼容池".into(),
            base_url: "https://api.openai.com/v1".into(),
            api_key: "sk-demo-openai".into(),
            protocol: UpstreamProtocol::ChatCompletions,
            supported_models: vec!["gpt-4.1-mini".into(), "gpt-4.1".into()],
            model_aliases: vec![],
            request_quota_5h: 600,
            requests_per_minute: 20,
            max_concurrency: 4,
            model_request_costs: vec![ModelRequestCostConfig {
                slug: "gpt-4.1-mini".into(),
                cost: 1,
            }],
            active: true,
            failure_count: 1,
        },
        UpstreamConfig {
            id: "up-backup-chat".into(),
            name: "Backup Chat".into(),
            base_url: "https://backup.example.com/v1".into(),
            api_key: "sk-demo-backup".into(),
            protocol: UpstreamProtocol::ChatCompletions,
            supported_models: vec!["moonshot-v1".into()],
            model_aliases: vec![],
            request_quota_5h: 300,
            requests_per_minute: 10,
            max_concurrency: 2,
            model_request_costs: vec![],
            active: false,
            failure_count: 2,
        },
    ]
}

fn sample_downstreams() -> Vec<DownstreamConfig> {
    vec![
        DownstreamConfig {
            id: "down-team-a".into(),
            name: "Team A".into(),
            hash: "sha256:demo-team-a".into(),
            plaintext_key: Some("sk-team-a-demo".into()),
            model_allowlist: vec!["glm-5".into(), "glm-5.1".into(), "gpt-4.1-mini".into()],
            per_minute_limit: 20,
            daily_token_limit: Some(100_000),
            monthly_token_limit: Some(200_000),
            request_quota_window_hours: None,
            request_quota_requests: None,
            ip_allowlist: vec!["127.0.0.1".into()],
            expires_at: None,
            active: true,
        },
        DownstreamConfig {
            id: "down-team-b".into(),
            name: "Team B".into(),
            hash: "sha256:demo-team-b".into(),
            plaintext_key: Some("sk-team-b-demo".into()),
            model_allowlist: vec!["gpt-4.1-mini".into()],
            per_minute_limit: 12,
            daily_token_limit: Some(50_000),
            monthly_token_limit: Some(100_000),
            request_quota_window_hours: Some(6),
            request_quota_requests: Some(400),
            ip_allowlist: vec![],
            expires_at: Some(1_725_000_000),
            active: true,
        },
        DownstreamConfig {
            id: "down-legacy-lab".into(),
            name: "Legacy Lab".into(),
            hash: "sha256:demo-legacy".into(),
            plaintext_key: Some("sk-legacy-demo".into()),
            model_allowlist: vec!["moonshot-v1".into()],
            per_minute_limit: 5,
            daily_token_limit: Some(10_000),
            monthly_token_limit: Some(20_000),
            request_quota_window_hours: None,
            request_quota_requests: None,
            ip_allowlist: vec!["10.0.0.0/24".into()],
            expires_at: Some(1_724_900_000),
            active: false,
        },
    ]
}

fn sample_usage_logs() -> Vec<UsageLog> {
    vec![
        UsageLog {
            id: "log-1041".into(),
            downstream_key_id: "down-team-a".into(),
            upstream_key_id: "up-glm-primary".into(),
            endpoint: "/v1/responses".into(),
            model: "glm-5".into(),
            request_id: "REQ-1041".into(),
            status_code: 200,
            prompt_tokens: 540,
            completion_tokens: 120,
            total_tokens: 660,
            latency_ms: 12,
            created_at: 1_725_000_100,
        },
        UsageLog {
            id: "log-1042".into(),
            downstream_key_id: "down-team-b".into(),
            upstream_key_id: "up-openai-compat".into(),
            endpoint: "/v1/chat/completions".into(),
            model: "gpt-4.1-mini".into(),
            request_id: "REQ-1042".into(),
            status_code: 200,
            prompt_tokens: 420,
            completion_tokens: 180,
            total_tokens: 600,
            latency_ms: 18,
            created_at: 1_725_000_200,
        },
        UsageLog {
            id: "log-1043".into(),
            downstream_key_id: "down-team-a".into(),
            upstream_key_id: "up-glm-primary".into(),
            endpoint: "/v1/responses".into(),
            model: "glm-5.1".into(),
            request_id: "REQ-1043".into(),
            status_code: 429,
            prompt_tokens: 1_100,
            completion_tokens: 0,
            total_tokens: 1_100,
            latency_ms: 24,
            created_at: 1_725_000_300,
        },
        UsageLog {
            id: "log-1044".into(),
            downstream_key_id: "down-legacy-lab".into(),
            upstream_key_id: "up-backup-chat".into(),
            endpoint: "/v1/chat/completions".into(),
            model: "moonshot-v1".into(),
            request_id: "REQ-1044".into(),
            status_code: 502,
            prompt_tokens: 260,
            completion_tokens: 0,
            total_tokens: 260,
            latency_ms: 9,
            created_at: 1_725_000_400,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upstreams_context_prefills_matching_edit_target() {
        let (_config, _state, form, _notice, form_open) = upstreams_context(Some("up-glm-primary"));

        assert!(form_open);
        assert_eq!(form.heading, "编辑上游");
        assert_eq!(form.action, "/admin/upstreams/up-glm-primary");
        assert_eq!(form.name, "GLM 主账号");
    }

    #[test]
    fn upstreams_context_falls_back_when_edit_target_is_missing() {
        let (_config, _state, form, _notice, form_open) = upstreams_context(Some("missing"));

        assert!(!form_open);
        assert_eq!(form.heading, "新增上游");
        assert_eq!(form.action, "/admin/upstreams");
    }

    #[test]
    fn downstreams_context_prefills_matching_edit_target() {
        let query = DownstreamListQuery {
            search: Some("team".into()),
            status: Some("active".into()),
            lifetime: Some("unlimited".into()),
        };
        let (_config, _state, form, returned_query, _notice, form_open) =
            downstreams_context(Some("down-team-a"), query.clone());

        assert!(form_open);
        assert_eq!(returned_query, query);
        assert_eq!(form.heading, "编辑下游密钥");
        assert_eq!(form.action, "/admin/downstreams/down-team-a");
        assert_eq!(form.name, "Team A");
    }

    #[test]
    fn downstreams_context_falls_back_when_edit_target_is_missing() {
        let query = DownstreamListQuery::default();
        let (_config, _state, form, returned_query, _notice, form_open) =
            downstreams_context(Some("missing"), query.clone());

        assert!(!form_open);
        assert_eq!(returned_query, query);
        assert_eq!(form.heading, "创建下游密钥");
        assert_eq!(form.action, "/admin/downstreams");
    }
}
