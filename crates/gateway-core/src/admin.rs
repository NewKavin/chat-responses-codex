use crate::routing::UpstreamProtocol;
use crate::state::{
    default_upstream_max_concurrency, default_upstream_request_quota_requests,
    default_upstream_request_quota_window_hours, default_upstream_requests_per_minute,
    DownstreamConfig, ModelRequestCostConfig, UpstreamConfig,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpstreamForm {
    pub intent: Option<String>,
    pub name: String,
    pub base_url: String,
    pub api_key: String,
    pub protocol: String,
    pub models: String,
    pub request_quota_window_hours: Option<u32>,
    pub request_quota_requests: Option<u32>,
    pub requests_per_minute: Option<u32>,
    pub max_concurrency: Option<u32>,
    pub model_request_costs: Option<String>,
    pub active: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownstreamForm {
    pub name: String,
    pub models: String,
    pub limit_mode: Option<String>,
    pub per_minute_limit: Option<u32>,
    pub daily_token_limit: Option<u64>,
    pub monthly_token_limit: Option<u64>,
    pub request_quota_window_hours: Option<u32>,
    pub request_quota_requests: Option<u32>,
    pub ip_allowlist: Option<String>,
    pub expires_at: Option<String>,
    pub never_expires: Option<String>,
    pub active: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownstreamListQuery {
    pub search: Option<String>,
    pub status: Option<String>,
    pub lifetime: Option<String>,
}

/// Returned by `DownstreamListQuery::status_filter`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DownstreamStatusFilter {
    All,
    Active,
    Inactive,
}

/// Returned by `DownstreamListQuery::lifetime_filter`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DownstreamLifetimeFilter {
    All,
    Unlimited,
    Expiring,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownstreamFormView {
    pub action: String,
    pub heading: String,
    pub submit_label: String,
    pub delete_action: Option<String>,
    pub rotate_action: Option<String>,
    pub id: Option<String>,
    pub name: String,
    pub models: String,
    pub limit_mode: String,
    pub per_minute_limit: String,
    pub daily_token_limit: String,
    pub monthly_token_limit: String,
    pub request_quota_window_hours: String,
    pub request_quota_requests: String,
    pub ip_allowlist: String,
    pub expires_at: String,
    pub never_expires: bool,
    pub active: bool,
    pub plaintext_key: Option<String>,
    pub plaintext_key_prefix: Option<String>,
    pub legacy_secret: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpstreamFormView {
    pub action: String,
    pub heading: String,
    pub submit_label: String,
    pub name: String,
    pub base_url: String,
    pub api_key: String,
    pub protocol: UpstreamProtocol,
    pub models: String,
    pub request_quota_window_hours: String,
    pub request_quota_requests: String,
    pub requests_per_minute: String,
    pub max_concurrency: String,
    pub model_request_costs: String,
    pub active: bool,
}

impl DownstreamListQuery {
    pub fn search_value(&self) -> String {
        self.search
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or_default()
            .to_string()
    }

    pub fn status_filter(&self) -> DownstreamStatusFilter {
        match self.status.as_deref().map(str::trim) {
            Some("active") => DownstreamStatusFilter::Active,
            Some("inactive") => DownstreamStatusFilter::Inactive,
            _ => DownstreamStatusFilter::All,
        }
    }

    pub fn lifetime_filter(&self) -> DownstreamLifetimeFilter {
        match self.lifetime.as_deref().map(str::trim) {
            Some("unlimited") => DownstreamLifetimeFilter::Unlimited,
            Some("expiring") => DownstreamLifetimeFilter::Expiring,
            _ => DownstreamLifetimeFilter::All,
        }
    }

    pub fn matches(&self, downstream: &DownstreamConfig) -> bool {
        let search = self.search_value();
        if !search.is_empty() {
            let search = search.to_lowercase();
            let name_matches = downstream.name.to_lowercase().contains(&search);
            let secret_matches = downstream
                .plaintext_key
                .as_deref()
                .map(|secret| secret.to_lowercase().contains(&search))
                .unwrap_or(false);
            if !name_matches && !secret_matches {
                return false;
            }
        }

        match self.status_filter() {
            DownstreamStatusFilter::All => {}
            DownstreamStatusFilter::Active => {
                if !downstream.active {
                    return false;
                }
            }
            DownstreamStatusFilter::Inactive => {
                if downstream.active {
                    return false;
                }
            }
        }

        match self.lifetime_filter() {
            DownstreamLifetimeFilter::All => {}
            DownstreamLifetimeFilter::Unlimited => {
                if downstream.expires_at.is_some() {
                    return false;
                }
            }
            DownstreamLifetimeFilter::Expiring => {
                if downstream.expires_at.is_none() {
                    return false;
                }
            }
        }

        true
    }

    pub fn normalized(&self) -> Self {
        Self {
            search: {
                let search = self.search_value();
                if search.is_empty() {
                    None
                } else {
                    Some(search)
                }
            },
            status: match self.status_filter() {
                DownstreamStatusFilter::Active => Some("active".to_string()),
                DownstreamStatusFilter::Inactive => Some("inactive".to_string()),
                DownstreamStatusFilter::All => None,
            },
            lifetime: match self.lifetime_filter() {
                DownstreamLifetimeFilter::Unlimited => Some("unlimited".to_string()),
                DownstreamLifetimeFilter::Expiring => Some("expiring".to_string()),
                DownstreamLifetimeFilter::All => None,
            },
        }
    }

    pub fn query_suffix(&self) -> String {
        let query = self.normalized();
        let encoded = serde_urlencoded::to_string(&query).unwrap_or_default();
        if encoded.is_empty() {
            String::new()
        } else {
            format!("?{encoded}")
        }
    }
}

impl DownstreamFormView {
pub fn blank() -> Self {
Self {
            action: "/admin/downstreams".to_string(),
            heading: "创建下游密钥".to_string(),
            submit_label: "创建密钥".to_string(),
            delete_action: None,
            rotate_action: None,
            id: None,
            name: String::new(),
            models: String::new(),
            limit_mode: "tokens".to_string(),
            per_minute_limit: "60".to_string(),
            daily_token_limit: String::new(),
            monthly_token_limit: String::new(),
            request_quota_window_hours: String::new(),
            request_quota_requests: String::new(),
            ip_allowlist: String::new(),
            expires_at: String::new(),
            never_expires: true,
            active: true,
            plaintext_key: None,
plaintext_key_prefix: None,
            legacy_secret: false,
        }
    }

    pub fn from_downstream(downstream: &DownstreamConfig) -> Self {
        Self {
            action: format!("/admin/downstreams/{}", downstream.id),
            heading: "编辑下游密钥".to_string(),
            submit_label: "保存修改".to_string(),
            delete_action: Some(format!("/admin/downstreams/{}/delete", downstream.id)),
            rotate_action: Some(format!("/admin/downstreams/{}/rotate", downstream.id)),
            id: Some(downstream.id.clone()),
            name: downstream.name.clone(),
            models: downstream.model_allowlist.join(","),
            limit_mode: if downstream.uses_request_quota() {
                "requests".to_string()
            } else {
                "tokens".to_string()
            },
            per_minute_limit: downstream.per_minute_limit.to_string(),
            daily_token_limit: downstream
                .daily_token_limit
                .map(|value| value.to_string())
                .unwrap_or_default(),
            monthly_token_limit: downstream
                .monthly_token_limit
                .map(|value| value.to_string())
                .unwrap_or_default(),
            request_quota_window_hours: downstream
                .request_quota_window_hours
                .map(|value| value.to_string())
                .unwrap_or_default(),
            request_quota_requests: downstream
                .request_quota_requests
                .map(|value| value.to_string())
                .unwrap_or_default(),
            ip_allowlist: downstream.ip_allowlist.join(","),
            expires_at: downstream
                .expires_at
                .map(|value| value.to_string())
                .unwrap_or_default(),
            never_expires: downstream.expires_at.is_none(),
            active: downstream.active,
            plaintext_key: downstream.plaintext_key.clone(),
            plaintext_key_prefix: downstream.plaintext_key_prefix.clone(),
            legacy_secret: downstream.plaintext_key.is_none(),
        }
    }

    pub fn from_form(
        form: &DownstreamForm,
        action: String,
        downstream_id: Option<String>,
        secret: Option<String>,
    ) -> Self {
        let is_editing = downstream_id.is_some();
        Self {
            action,
            heading: if is_editing {
                "编辑下游密钥".to_string()
            } else {
                "创建下游密钥".to_string()
            },
            submit_label: if is_editing {
                "保存修改".to_string()
            } else {
                "创建密钥".to_string()
            },
            delete_action: downstream_id
                .as_ref()
                .map(|value| format!("/admin/downstreams/{value}/delete")),
            rotate_action: downstream_id
                .as_ref()
                .map(|value| format!("/admin/downstreams/{value}/rotate")),
            id: downstream_id,
            name: form.name.clone(),
            models: form.models.clone(),
            limit_mode: form
                .limit_mode
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("tokens")
                .to_string(),
            per_minute_limit: form
                .per_minute_limit
                .map(|value| value.to_string())
                .unwrap_or_else(|| "60".to_string()),
            daily_token_limit: form
                .daily_token_limit
                .map(|value| value.to_string())
                .unwrap_or_default(),
            monthly_token_limit: form
                .monthly_token_limit
                .map(|value| value.to_string())
                .unwrap_or_default(),
            request_quota_window_hours: form
                .request_quota_window_hours
                .map(|value| value.to_string())
                .unwrap_or_default(),
            request_quota_requests: form
                .request_quota_requests
                .map(|value| value.to_string())
                .unwrap_or_default(),
            ip_allowlist: form.ip_allowlist.clone().unwrap_or_default(),
            expires_at: if form.never_expires.is_some() {
                String::new()
            } else {
                form.expires_at
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or_default()
                    .to_string()
            },
            never_expires: form.never_expires.is_some()
                || form
                    .expires_at
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or_default()
                    .is_empty(),
            active: form_toggle_enabled(&form.active),
            plaintext_key: secret.clone(),
            plaintext_key_prefix: None,
            legacy_secret: secret.is_none(),
        }
    }
}

impl UpstreamFormView {
    pub fn blank() -> Self {
        Self {
            action: "/admin/upstreams".to_string(),
            heading: "新增上游".to_string(),
            submit_label: "保存上游".to_string(),
            name: String::new(),
            base_url: String::new(),
            api_key: String::new(),
            protocol: UpstreamProtocol::ChatCompletions,
            models: String::new(),
            request_quota_window_hours: default_upstream_request_quota_window_hours().to_string(),
            request_quota_requests: default_upstream_request_quota_requests().to_string(),
            requests_per_minute: default_upstream_requests_per_minute().to_string(),
            max_concurrency: default_upstream_max_concurrency().to_string(),
            model_request_costs: String::new(),
            active: true,
        }
    }

    pub fn from_upstream(upstream: &UpstreamConfig) -> Self {
        Self {
            action: format!("/admin/upstreams/{}", upstream.id),
            heading: "编辑上游".to_string(),
            submit_label: "保存修改".to_string(),
            name: upstream.name.clone(),
            base_url: upstream.base_url.clone(),
            api_key: upstream.api_key.clone(),
            protocol: upstream.protocol,
            models: upstream.route_models().join(","),
            request_quota_window_hours: upstream.request_quota_window_hours.to_string(),
            request_quota_requests: upstream.request_quota_requests.to_string(),
            requests_per_minute: upstream.requests_per_minute.to_string(),
            max_concurrency: upstream.max_concurrency.to_string(),
            model_request_costs: format_model_request_costs(&upstream.model_request_costs),
            active: upstream.active,
        }
    }

    pub fn from_form(
        form: &UpstreamForm,
        action: String,
        existing: Option<&UpstreamConfig>,
    ) -> Self {
        let is_editing = action != "/admin/upstreams";
        let existing_request_quota_window_hours =
            existing.map(|upstream| upstream.request_quota_window_hours);
        let existing_request_quota_requests = existing.map(|upstream| upstream.request_quota_requests);
        let existing_requests_per_minute = existing.map(|upstream| upstream.requests_per_minute);
        let existing_max_concurrency = existing.map(|upstream| upstream.max_concurrency);
        Self {
            action,
            heading: if is_editing {
                "编辑上游".to_string()
            } else {
                "新增上游".to_string()
            },
            submit_label: if is_editing {
                "保存修改".to_string()
            } else {
                "保存上游".to_string()
            },
            name: form.name.clone(),
            base_url: form.base_url.clone(),
            api_key: form.api_key.clone(),
            protocol: parse_upstream_protocol(&form.protocol),
            models: form.models.clone(),
            request_quota_window_hours: upstream_form_u32_string(
                form.request_quota_window_hours,
                existing_request_quota_window_hours,
                default_upstream_request_quota_window_hours(),
            ),
            request_quota_requests: upstream_form_u32_string(
                form.request_quota_requests,
                existing_request_quota_requests,
                default_upstream_request_quota_requests(),
            ),
            requests_per_minute: upstream_form_u32_string(
                form.requests_per_minute,
                existing_requests_per_minute,
                default_upstream_requests_per_minute(),
            ),
            max_concurrency: upstream_form_u32_string(
                form.max_concurrency,
                existing_max_concurrency,
                default_upstream_max_concurrency(),
            ),
            model_request_costs: form
                .model_request_costs
                .clone()
                .or_else(|| {
                    existing
                        .map(|upstream| format_model_request_costs(&upstream.model_request_costs))
                })
                .unwrap_or_default(),
            active: form_toggle_enabled(&form.active),
        }
    }

    pub fn with_fetched_models(&self, models: String) -> Self {
        let mut next = self.clone();
        next.models = models;
        next
    }
}

pub fn normalize_fetched_models(models: Vec<String>) -> String {
    let mut seen = std::collections::HashSet::new();
    let mut normalized_models = Vec::new();

    for model in models {
        let original = model.trim();
        if original.is_empty() {
            continue;
        }
        if seen.insert(original.to_string()) {
            normalized_models.push(original.to_string());
        }
    }

    normalized_models.join(",")
}

fn parse_upstream_protocol(value: &str) -> UpstreamProtocol {
    match value {
        "responses" => UpstreamProtocol::Responses,
        _ => UpstreamProtocol::ChatCompletions,
    }
}

fn form_toggle_enabled(value: &Option<String>) -> bool {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some()
}

fn format_model_request_costs(costs: &[ModelRequestCostConfig]) -> String {
    if costs.is_empty() {
        return String::new();
    }

    costs
        .iter()
        .map(|rule| format!("{}={}", rule.slug, rule.cost))
        .collect::<Vec<_>>()
        .join("\n")
}

fn upstream_form_u32_string(value: Option<u32>, existing: Option<u32>, default: u32) -> String {
    value.or(existing).unwrap_or(default).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ModelRequestCostConfig;

    fn upstream_config() -> UpstreamConfig {
        UpstreamConfig {
            id: "up-1".into(),
            name: "Primary Upstream".into(),
            base_url: "https://upstream.example.com".into(),
            api_key: "up-key".into(),
            protocol: UpstreamProtocol::Responses,
            supported_models: vec!["glm-5".into()],
            request_quota_window_hours: 5,
            request_quota_requests: 11,
            requests_per_minute: 22,
            max_concurrency: 33,
            model_request_costs: vec![ModelRequestCostConfig {
                slug: "glm-5".into(),
                cost: 2,
            }],
            active: false,
            failure_count: 0,
        }
    }

    fn downstream_config() -> DownstreamConfig {
        DownstreamConfig {
            id: "down-1".into(),
            name: "Portal Client".into(),
            hash: "hash".into(),
            plaintext_key: Some("secret-key".into()),
            model_allowlist: vec!["glm-5".into(), "glm-5.1".into()],
            per_minute_limit: 120,
            rate_limit_enabled: true,
            max_concurrency: 10,
            daily_token_limit: Some(1_000),
            monthly_token_limit: None,
            request_quota_window_hours: None,
            request_quota_requests: None,
            ip_allowlist: vec!["127.0.0.1".into()],
            expires_at: Some(1_725_000_000),
            active: false,
        }
    }

    #[test]
    fn downstream_query_helpers_work() {
        let query = DownstreamListQuery {
            search: Some("  secret  ".into()),
            status: Some("inactive".into()),
            lifetime: Some("expiring".into()),
        };

        assert_eq!(query.search_value(), "secret");
        assert_eq!(query.status_filter(), DownstreamStatusFilter::Inactive);
        assert_eq!(query.lifetime_filter(), DownstreamLifetimeFilter::Expiring);
        assert_eq!(
            query.normalized(),
            DownstreamListQuery {
                search: Some("secret".into()),
                status: Some("inactive".into()),
                lifetime: Some("expiring".into()),
            }
        );
        assert_eq!(
            query.query_suffix(),
            "?search=secret&status=inactive&lifetime=expiring"
        );
        assert!(query.matches(&downstream_config()));
    }

    #[test]
    fn upstream_form_view_merges_fetched_models() {
        let form = UpstreamForm {
            intent: Some("fetch".into()),
            name: "New Upstream".into(),
            base_url: "https://new.example.com".into(),
            api_key: "new-key".into(),
            protocol: "responses".into(),
            models: "glm-5".into(),
            request_quota_window_hours: None,
            request_quota_requests: None,
            requests_per_minute: Some(88),
            max_concurrency: None,
            model_request_costs: None,
            active: Some("on".into()),
        };

        let view = UpstreamFormView::from_form(
            &form,
            "/admin/upstreams/1".into(),
            Some(&upstream_config()),
        );
        assert_eq!(view.heading, "编辑上游");
        assert_eq!(view.protocol, UpstreamProtocol::Responses);
        assert_eq!(view.request_quota_window_hours, "5");
        assert_eq!(view.request_quota_requests, "11");
        assert_eq!(view.requests_per_minute, "88");
        assert_eq!(view.max_concurrency, "33");
        assert_eq!(view.model_request_costs, "glm-5=2");

        let updated = view.with_fetched_models("GLM-5,GLM-5.1".into());
        assert_eq!(updated.models, "glm-5,glm-5.1");
    }

    #[test]
    fn normalize_fetched_models_preserves_exact_model_names() {
        let models = normalize_fetched_models(vec![
            "GLM-5".into(),
            "GLM-5".into(),
            "GLM-5.1".into(),
        ]);

        assert_eq!(models, "GLM-5,GLM-5.1");
    }

    #[test]
    fn downstream_form_view_populates_edit_and_create_states() {
        let edited = DownstreamFormView::from_downstream(&downstream_config());
        assert_eq!(edited.action, "/admin/downstreams/down-1");
        assert_eq!(
            edited.delete_action.as_deref(),
            Some("/admin/downstreams/down-1/delete")
        );
        assert_eq!(
            edited.rotate_action.as_deref(),
            Some("/admin/downstreams/down-1/rotate")
        );
        assert_eq!(edited.per_minute_limit, "120");
        assert_eq!(edited.daily_token_limit, "1000");
        assert_eq!(edited.monthly_token_limit, "");
        assert_eq!(edited.expires_at, "1725000000");

        let form = DownstreamForm {
            name: "Portal Client".into(),
            models: "glm-5,glm-5.1".into(),
            limit_mode: Some("tokens".into()),
            per_minute_limit: None,
            daily_token_limit: Some(500),
            monthly_token_limit: Some(1_500),
            request_quota_window_hours: None,
            request_quota_requests: None,
            ip_allowlist: Some("10.0.0.1".into()),
            expires_at: Some("2026-01-01".into()),
            never_expires: Some("on".into()),
            active: Some("on".into()),
        };
        let created = DownstreamFormView::from_form(
            &form,
            "/admin/downstreams".into(),
            None,
            Some("generated-secret".into()),
        );
        assert_eq!(created.heading, "创建下游密钥");
        assert_eq!(created.per_minute_limit, "60");
        assert_eq!(created.expires_at, "");
        assert!(created.never_expires);
        assert!(created.active);
        assert_eq!(created.plaintext_key.as_deref(), Some("generated-secret"));
    }

    #[test]
    fn downstream_form_view_keeps_token_values_as_reference_data_for_request_quota_downstreams() {
        let mut downstream = downstream_config();
        downstream.request_quota_window_hours = Some(6);
        downstream.request_quota_requests = Some(300);
        downstream.daily_token_limit = Some(1_000);
        downstream.monthly_token_limit = Some(2_000);

        let view = DownstreamFormView::from_downstream(&downstream);

        assert_eq!(view.limit_mode, "requests");
        assert_eq!(view.daily_token_limit, "1000");
        assert_eq!(view.monthly_token_limit, "2000");
        assert_eq!(view.request_quota_window_hours, "6");
        assert_eq!(view.request_quota_requests, "300");
    }
}
