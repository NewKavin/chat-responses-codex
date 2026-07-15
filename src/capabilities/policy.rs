use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};

use globset::{Glob, GlobMatcher};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::types::{
    AgentClientProfile, Capability, CapabilityConfiguration, CapabilityPolicy, CapabilitySelector,
    CompatibilityExpectation, DeclarativeProbeCase, HttpsImageFixture, ProbeCandidates,
    RouteCapabilityOverride, RouteIdentity, SemanticPolicy,
};
use super::{
    CAPABILITY_SCHEMA_VERSION, MAX_CAPABILITY_COLLECTION_ENTRIES,
    MAX_CAPABILITY_CONFIGURATION_BYTES, MAX_CAPABILITY_EXTENSION_CASE_BYTES,
    MAX_CAPABILITY_EXTENSION_PROBES, MAX_CAPABILITY_SELECTOR_VALUE_BYTES,
};

const MAX_PREDICATE_PATH_BYTES: usize = 256;
const PROTECTED_REQUEST_PATHS: [&str; 10] = [
    "/model",
    "/messages",
    "/input",
    "/tools",
    "/stream",
    "/headers",
    "/url",
    "/image_url",
    "/source",
    "/data",
];

#[derive(Debug, Error)]
pub enum CapabilityPolicyError {
    #[error("unsupported schema version {found}; expected {expected}")]
    UnsupportedSchemaVersion { found: u32, expected: u32 },
    #[error("empty capability id")]
    EmptyCapabilityId,
    #[error("duplicate capability id {id}")]
    DuplicateCapabilityId { id: String },
    #[error("unknown compatibility bundle {bundle} in expectation {expectation}")]
    UnknownCompatibilityBundle { expectation: String, bundle: String },
    #[error("invalid selector glob {pattern} for {id}: {reason}")]
    InvalidSelectorGlob {
        id: String,
        pattern: String,
        reason: String,
    },
    #[error("ambiguous semantic field {field} between {first} and {second}")]
    AmbiguousSemanticField {
        field: String,
        first: String,
        second: String,
    },
    #[error("ambiguous override field {field} between {first} and {second}")]
    AmbiguousOverrideField {
        field: String,
        first: String,
        second: String,
    },
    #[error("protected request path {path} in extension case {id}")]
    ProtectedRequestPath { id: String, path: String },
    #[error(
        "invalid HTTPS fixture for {id}: URL must use HTTPS and expected label must be non-empty"
    )]
    InvalidHttpsFixture { id: String },
    #[error("sensitive URL is not permitted in capability configuration")]
    SensitiveUrl,
    #[error("invalid bounded response predicate path {path} in extension case {id}")]
    InvalidBoundedResponsePredicatePath { id: String, path: String },
    #[error("extension case over 16384 serialized bytes: {id} is at least {size} bytes")]
    ExtensionCaseTooLarge { id: String, size: usize },
    #[error("route tag assignment cannot select by tag: {id}")]
    RouteTagAssignmentSelectsByTag { id: String },
    #[error("route tag assignment cannot assign empty tags: {id}")]
    RouteTagAssignmentHasEmptyTags { id: String },
    #[error("configuration exceeds {maximum} bytes")]
    CapabilityConfigurationTooLarge { maximum: usize },
    #[error("too many entries in field {field}: {count} exceeds maximum {maximum}")]
    TooManyEntries {
        field: &'static str,
        count: usize,
        maximum: usize,
    },
    #[error("selector value exceeds {maximum} bytes in field {field} for {id}: {length} bytes")]
    SelectorValueTooLong {
        id: String,
        field: &'static str,
        length: usize,
        maximum: usize,
    },
    #[error("failed to serialize capability configuration: {0}")]
    Serialization(#[from] serde_json::Error),
}

struct BoundedWriter {
    bytes_written: usize,
    maximum: usize,
    exceeded: bool,
}

impl BoundedWriter {
    fn new(maximum: usize) -> Self {
        Self {
            bytes_written: 0,
            maximum,
            exceeded: false,
        }
    }
}

impl Write for BoundedWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.len() > self.maximum.saturating_sub(self.bytes_written) {
            self.exceeded = true;
            return Err(io::Error::other("serialized size limit exceeded"));
        }
        self.bytes_written += buffer.len();
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct Sha256Writer(Sha256);

impl Write for Sha256Writer {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0.update(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct CompiledSelector {
    source: CapabilitySelector,
    runtime_model_glob: Option<GlobMatcher>,
    specificity: u8,
}

impl CompiledSelector {
    fn compile(id: &str, selector: &CapabilitySelector) -> Result<Self, CapabilityPolicyError> {
        let runtime_model_glob = selector
            .runtime_model_glob
            .as_deref()
            .map(|pattern| {
                Glob::new(pattern)
                    .map(|glob| glob.compile_matcher())
                    .map_err(|error| CapabilityPolicyError::InvalidSelectorGlob {
                        id: id.to_owned(),
                        pattern: pattern.to_owned(),
                        reason: error.to_string(),
                    })
            })
            .transpose()?;
        let exact_fields = [
            selector.exposed_model.is_some(),
            selector.runtime_model.is_some(),
            selector.upstream_id.is_some(),
            selector.protocol.is_some(),
            selector.tag.is_some(),
        ]
        .into_iter()
        .filter(|present| *present)
        .count() as u8;

        Ok(Self {
            source: selector.clone(),
            runtime_model_glob,
            specificity: exact_fields * 2 + u8::from(selector.runtime_model_glob.is_some()),
        })
    }

    fn matches(&self, route: &RouteIdentity) -> bool {
        optional_matches(
            self.source.exposed_model.as_deref(),
            &route.exposed_model_slug,
        ) && optional_matches(
            self.source.runtime_model.as_deref(),
            &route.runtime_model_slug,
        ) && optional_matches(self.source.upstream_id.as_deref(), &route.upstream_id)
            && self
                .source
                .protocol
                .map(|protocol| protocol == route.protocol)
                .unwrap_or(true)
            && self
                .source
                .tag
                .as_ref()
                .map(|tag| route.tags.contains(tag))
                .unwrap_or(true)
            && self
                .runtime_model_glob
                .as_ref()
                .map(|glob| glob.is_match(&route.runtime_model_slug))
                .unwrap_or(true)
    }

    fn is_satisfiable(&self) -> bool {
        match (
            self.source.runtime_model.as_deref(),
            self.runtime_model_glob.as_ref(),
        ) {
            (Some(runtime_model), Some(glob)) => glob.is_match(runtime_model),
            _ => true,
        }
    }
}

fn optional_matches(expected: Option<&str>, actual: &str) -> bool {
    expected.map(|expected| expected == actual).unwrap_or(true)
}

#[derive(Debug)]
struct CompiledPolicy {
    source_index: usize,
    selector: CompiledSelector,
}

#[derive(Debug)]
struct CompiledOverride {
    source_index: usize,
    selector: CompiledSelector,
}

#[derive(Debug)]
struct CompiledRouteTagAssignment {
    source_index: usize,
    selector: CompiledSelector,
}

#[derive(Clone, Debug)]
pub struct CompiledExpectation {
    pub id: String,
    pub required: BTreeSet<Capability>,
    pub client_profiles: BTreeSet<AgentClientProfile>,
    pub permitted_optional_downgrades: BTreeSet<String>,
    pub https_image_fixture: Option<HttpsImageFixture>,
    pub selector: CapabilitySelector,
    compiled_selector: CompiledSelector,
}

#[derive(Debug)]
pub struct CompiledCapabilityConfiguration {
    source: CapabilityConfiguration,
    digest: String,
    policies: Vec<CompiledPolicy>,
    route_overrides: Vec<CompiledOverride>,
    ordered_route_overrides: Vec<RouteCapabilityOverride>,
    route_tags: Vec<CompiledRouteTagAssignment>,
    expectations: Vec<CompiledExpectation>,
}

impl CapabilityConfiguration {
    pub fn compile(&self) -> Result<CompiledCapabilityConfiguration, CapabilityPolicyError> {
        if self.schema_version != CAPABILITY_SCHEMA_VERSION {
            return Err(CapabilityPolicyError::UnsupportedSchemaVersion {
                found: self.schema_version,
                expected: CAPABILITY_SCHEMA_VERSION,
            });
        }

        validate_resource_bounds(self)?;
        validate_ids(self)?;
        validate_fixtures(self)?;
        validate_extensions(self)?;
        validate_route_tags(self)?;

        let mut policies = self
            .policies
            .iter()
            .enumerate()
            .map(|(source_index, policy)| {
                Ok(CompiledPolicy {
                    source_index,
                    selector: CompiledSelector::compile(&policy.id, &policy.selector)?,
                })
            })
            .collect::<Result<Vec<_>, CapabilityPolicyError>>()?;
        let mut route_overrides = self
            .route_overrides
            .iter()
            .enumerate()
            .map(|(source_index, route_override)| {
                Ok(CompiledOverride {
                    source_index,
                    selector: CompiledSelector::compile(
                        &route_override.id,
                        &route_override.selector,
                    )?,
                })
            })
            .collect::<Result<Vec<_>, CapabilityPolicyError>>()?;
        let route_tags = self
            .route_tags
            .iter()
            .enumerate()
            .map(|(source_index, assignment)| {
                Ok(CompiledRouteTagAssignment {
                    source_index,
                    selector: CompiledSelector::compile(&assignment.id, &assignment.selector)?,
                })
            })
            .collect::<Result<Vec<_>, CapabilityPolicyError>>()?;
        let expectations = compile_expectations(self)?;

        validate_policy_conflicts(self, &policies)?;
        validate_override_conflicts(self, &route_overrides)?;

        policies.sort_by(|left, right| {
            policy_rank(self, left)
                .cmp(&policy_rank(self, right))
                .then_with(|| {
                    self.policies[left.source_index]
                        .id
                        .cmp(&self.policies[right.source_index].id)
                })
        });
        route_overrides.sort_by(|left, right| {
            override_rank(self, left)
                .cmp(&override_rank(self, right))
                .then_with(|| {
                    self.route_overrides[left.source_index]
                        .id
                        .cmp(&self.route_overrides[right.source_index].id)
                })
        });
        let ordered_route_overrides = route_overrides
            .iter()
            .map(|route_override| self.route_overrides[route_override.source_index].clone())
            .collect();

        let mut digest_writer = Sha256Writer(Sha256::new());
        serde_json::to_writer(&mut digest_writer, self)?;
        let digest = digest_writer
            .0
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();

        Ok(CompiledCapabilityConfiguration {
            source: self.clone(),
            digest,
            policies,
            route_overrides,
            ordered_route_overrides,
            route_tags,
            expectations,
        })
    }
}

impl CompiledCapabilityConfiguration {
    pub fn source(&self) -> &CapabilityConfiguration {
        &self.source
    }

    pub fn digest(&self) -> &str {
        &self.digest
    }

    pub fn expectations(&self) -> &[CompiledExpectation] {
        &self.expectations
    }

    pub fn expectations_for(&self, route: &RouteIdentity) -> Vec<&CompiledExpectation> {
        self.expectations
            .iter()
            .filter(|expectation| expectation.compiled_selector.matches(route))
            .collect()
    }

    pub fn route_overrides(&self) -> &[RouteCapabilityOverride] {
        &self.ordered_route_overrides
    }

    pub fn route_overrides_for(&self, route: &RouteIdentity) -> Vec<&RouteCapabilityOverride> {
        self.route_overrides
            .iter()
            .filter(|route_override| route_override.selector.matches(route))
            .map(|route_override| &self.source.route_overrides[route_override.source_index])
            .collect()
    }

    pub fn apply_route_tags(&self, route: &mut RouteIdentity) {
        for assignment in &self.route_tags {
            if assignment.selector.matches(route) {
                route.tags.extend(
                    self.source.route_tags[assignment.source_index]
                        .tags
                        .iter()
                        .cloned(),
                );
            }
        }
    }

    pub fn semantic_for(&self, route: &RouteIdentity) -> SemanticPolicy {
        let mut semantic = SemanticPolicy::default();
        for policy in self.matching_policies(route) {
            let source = &self.source.policies[policy.source_index].semantic;
            if source.reasoning_mode.is_some() {
                semantic.reasoning_mode = source.reasoning_mode;
            }
            if source.reasoning_replay_required.is_some() {
                semantic.reasoning_replay_required = source.reasoning_replay_required;
            }
            if source.context_window.is_some() {
                semantic.context_window = source.context_window;
            }
            if source.max_output_tokens.is_some() {
                semantic.max_output_tokens = source.max_output_tokens;
            }
            semantic.effort_map.extend(source.effort_map.clone());
            semantic
                .omit_sampling_fields
                .extend(source.omit_sampling_fields.iter().cloned());
        }
        semantic
    }

    pub fn extensions_for(&self, route: &RouteIdentity) -> Vec<&DeclarativeProbeCase> {
        self.matching_policies(route)
            .flat_map(|policy| {
                self.source.policies[policy.source_index]
                    .extension_probes
                    .iter()
            })
            .collect()
    }

    pub fn probe_candidates_for(&self, route: &RouteIdentity) -> ProbeCandidates {
        let mut candidates = ProbeCandidates::default();
        for policy in self.matching_policies(route) {
            let source = &self.source.policies[policy.source_index].probe_candidates;
            for &field in &source.token_limit_fields {
                if !candidates.token_limit_fields.contains(&field) {
                    candidates.token_limit_fields.push(field);
                }
            }
            for (field, values) in &source.reasoning_controls {
                let accepted = candidates
                    .reasoning_controls
                    .entry(field.clone())
                    .or_default();
                for value in values {
                    if !accepted.contains(value) {
                        accepted.push(value.clone());
                    }
                }
            }
            for &carrier in &source.reasoning_carriers {
                if !candidates.reasoning_carriers.contains(&carrier) {
                    candidates.reasoning_carriers.push(carrier);
                }
            }
        }
        candidates
    }

    pub fn policy_ids_for(&self, route: &RouteIdentity) -> Vec<&str> {
        self.matching_policies(route)
            .map(|policy| self.source.policies[policy.source_index].id.as_str())
            .collect()
    }

    fn matching_policies<'a>(
        &'a self,
        route: &'a RouteIdentity,
    ) -> impl Iterator<Item = &'a CompiledPolicy> + 'a {
        self.policies
            .iter()
            .filter(move |policy| policy.selector.matches(route))
    }
}

fn validate_resource_bounds(
    configuration: &CapabilityConfiguration,
) -> Result<(), CapabilityPolicyError> {
    for (field, count) in [
        ("policies", configuration.policies.len()),
        ("route_overrides", configuration.route_overrides.len()),
        ("route_tags", configuration.route_tags.len()),
        ("bundles", configuration.bundles.len()),
        (
            "compatibility_expectations",
            configuration.compatibility_expectations.len(),
        ),
    ] {
        validate_entry_count(field, count, MAX_CAPABILITY_COLLECTION_ENTRIES)?;
    }

    let extension_probe_count = configuration
        .policies
        .iter()
        .map(|policy| policy.extension_probes.len())
        .try_fold(0usize, |count, policy_count| {
            let count = count.saturating_add(policy_count);
            if count > MAX_CAPABILITY_EXTENSION_PROBES {
                Err(CapabilityPolicyError::TooManyEntries {
                    field: "extension_probes",
                    count,
                    maximum: MAX_CAPABILITY_EXTENSION_PROBES,
                })
            } else {
                Ok(count)
            }
        })?;
    debug_assert!(extension_probe_count <= MAX_CAPABILITY_EXTENSION_PROBES);

    for policy in &configuration.policies {
        validate_selector_value_lengths(&policy.id, &policy.selector)?;
    }
    for route_override in &configuration.route_overrides {
        validate_selector_value_lengths(&route_override.id, &route_override.selector)?;
    }
    for assignment in &configuration.route_tags {
        validate_selector_value_lengths(&assignment.id, &assignment.selector)?;
        for tag in &assignment.tags {
            validate_selector_value_length(&assignment.id, "route_tags.tags", tag)?;
        }
    }
    for expectation in &configuration.compatibility_expectations {
        validate_selector_value_lengths(&expectation.id, &expectation.selector)?;
    }

    if bounded_serialized_size(configuration, MAX_CAPABILITY_CONFIGURATION_BYTES)?.is_none() {
        return Err(CapabilityPolicyError::CapabilityConfigurationTooLarge {
            maximum: MAX_CAPABILITY_CONFIGURATION_BYTES,
        });
    }
    Ok(())
}

fn validate_entry_count(
    field: &'static str,
    count: usize,
    maximum: usize,
) -> Result<(), CapabilityPolicyError> {
    if count > maximum {
        return Err(CapabilityPolicyError::TooManyEntries {
            field,
            count,
            maximum,
        });
    }
    Ok(())
}

fn validate_selector_value_lengths(
    id: &str,
    selector: &CapabilitySelector,
) -> Result<(), CapabilityPolicyError> {
    for (field, value) in [
        ("selector.exposed_model", selector.exposed_model.as_deref()),
        ("selector.runtime_model", selector.runtime_model.as_deref()),
        (
            "selector.runtime_model_glob",
            selector.runtime_model_glob.as_deref(),
        ),
        ("selector.upstream_id", selector.upstream_id.as_deref()),
        ("selector.tag", selector.tag.as_deref()),
    ] {
        if let Some(value) = value {
            validate_selector_value_length(id, field, value)?;
        }
    }
    Ok(())
}

fn validate_selector_value_length(
    id: &str,
    field: &'static str,
    value: &str,
) -> Result<(), CapabilityPolicyError> {
    if value.len() > MAX_CAPABILITY_SELECTOR_VALUE_BYTES {
        return Err(CapabilityPolicyError::SelectorValueTooLong {
            id: id.to_owned(),
            field,
            length: value.len(),
            maximum: MAX_CAPABILITY_SELECTOR_VALUE_BYTES,
        });
    }
    Ok(())
}

fn bounded_serialized_size<T: Serialize>(
    value: &T,
    maximum: usize,
) -> Result<Option<usize>, CapabilityPolicyError> {
    let mut writer = BoundedWriter::new(maximum);
    let result = serde_json::to_writer(&mut writer, value);
    if writer.exceeded {
        return Ok(None);
    }
    result?;
    Ok(Some(writer.bytes_written))
}

fn validate_ids(configuration: &CapabilityConfiguration) -> Result<(), CapabilityPolicyError> {
    let mut ids = BTreeSet::new();
    for id in configuration
        .policies
        .iter()
        .map(|item| item.id.as_str())
        .chain(
            configuration
                .route_overrides
                .iter()
                .map(|item| item.id.as_str()),
        )
        .chain(configuration.route_tags.iter().map(|item| item.id.as_str()))
        .chain(configuration.bundles.iter().map(|item| item.id.as_str()))
        .chain(
            configuration
                .compatibility_expectations
                .iter()
                .map(|item| item.id.as_str()),
        )
        .chain(
            configuration
                .policies
                .iter()
                .flat_map(|policy| policy.extension_probes.iter())
                .map(|item| item.id.as_str()),
        )
    {
        if id.trim().is_empty() {
            return Err(CapabilityPolicyError::EmptyCapabilityId);
        }
        if !ids.insert(id) {
            return Err(CapabilityPolicyError::DuplicateCapabilityId { id: id.to_owned() });
        }
    }
    Ok(())
}

fn validate_fixtures(configuration: &CapabilityConfiguration) -> Result<(), CapabilityPolicyError> {
    if let Some(fixture) = &configuration.probe.https_image_fixture {
        validate_fixture("probe", fixture)?;
    }
    for expectation in &configuration.compatibility_expectations {
        if let Some(fixture) = &expectation.https_image_fixture {
            validate_fixture(&expectation.id, fixture)?;
        }
    }
    for evidence in configuration
        .policies
        .iter()
        .flat_map(|policy| policy.evidence.iter())
    {
        validate_public_url(&evidence.url)?;
    }
    Ok(())
}

fn validate_fixture(id: &str, fixture: &HttpsImageFixture) -> Result<(), CapabilityPolicyError> {
    let valid_url = reqwest::Url::parse(&fixture.url)
        .ok()
        .filter(|url| url.scheme() == "https" && url.host_str().is_some())
        .is_some();
    if !valid_url || fixture.expected_label.trim().is_empty() {
        return Err(CapabilityPolicyError::InvalidHttpsFixture { id: id.to_owned() });
    }
    validate_public_url(&fixture.url)?;
    Ok(())
}

pub fn is_sensitive_url(value: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(value) else {
        return false;
    };
    if !url.username().is_empty() || url.password().is_some() {
        return true;
    }
    url.query_pairs()
        .any(|(key, _)| is_sensitive_url_query_key(&key))
}

pub fn sanitize_sensitive_urls(configuration: &mut CapabilityConfiguration) -> bool {
    let mut changed = sanitize_fixture_url(configuration.probe.https_image_fixture.as_mut());
    for expectation in &mut configuration.compatibility_expectations {
        changed |= sanitize_fixture_url(expectation.https_image_fixture.as_mut());
    }
    for policy in &mut configuration.policies {
        for evidence in &mut policy.evidence {
            changed |= sanitize_url(&mut evidence.url);
        }
        for extension in &mut policy.extension_probes {
            changed |= sanitize_json_urls(&mut extension.request_patch);
        }
    }
    changed
}

fn sanitize_fixture_url(fixture: Option<&mut HttpsImageFixture>) -> bool {
    fixture
        .map(|fixture| sanitize_url(&mut fixture.url))
        .unwrap_or(false)
}

fn sanitize_url(value: &mut String) -> bool {
    if !is_sensitive_url(value) {
        return false;
    }
    *value = "https://redacted.invalid/".into();
    true
}

fn sanitize_json_urls(value: &mut Value) -> bool {
    match value {
        Value::Object(object) => object
            .values_mut()
            .fold(false, |changed, value| sanitize_json_urls(value) | changed),
        Value::Array(values) => values
            .iter_mut()
            .fold(false, |changed, value| sanitize_json_urls(value) | changed),
        Value::String(value) => sanitize_url(value),
        _ => false,
    }
}

fn is_sensitive_url_query_key(key: &str) -> bool {
    let normalized = key
        .bytes()
        .filter_map(|byte| {
            byte.is_ascii_alphanumeric()
                .then_some(byte.to_ascii_lowercase())
        })
        .collect::<Vec<_>>();
    let normalized = String::from_utf8_lossy(&normalized);
    [
        "token",
        "key",
        "secret",
        "signature",
        "sig",
        "credential",
        "password",
        "passwd",
        "auth",
    ]
    .iter()
    .any(|term| normalized.contains(term))
}

fn validate_public_url(value: &str) -> Result<(), CapabilityPolicyError> {
    if is_sensitive_url(value) {
        return Err(CapabilityPolicyError::SensitiveUrl);
    }
    Ok(())
}

fn validate_extensions(
    configuration: &CapabilityConfiguration,
) -> Result<(), CapabilityPolicyError> {
    for case in configuration
        .policies
        .iter()
        .flat_map(|policy| &policy.extension_probes)
    {
        if bounded_serialized_size(case, MAX_CAPABILITY_EXTENSION_CASE_BYTES)?.is_none() {
            return Err(CapabilityPolicyError::ExtensionCaseTooLarge {
                id: case.id.clone(),
                size: MAX_CAPABILITY_EXTENSION_CASE_BYTES + 1,
            });
        }
        let predicate_path = &case.response_predicate.path;
        if !predicate_path.starts_with('/') || predicate_path.len() > MAX_PREDICATE_PATH_BYTES {
            return Err(CapabilityPolicyError::InvalidBoundedResponsePredicatePath {
                id: case.id.clone(),
                path: predicate_path.clone(),
            });
        }
        validate_request_patch(case, &case.request_patch, "")?;
    }
    Ok(())
}

fn validate_request_patch(
    case: &DeclarativeProbeCase,
    value: &Value,
    parent_path: &str,
) -> Result<(), CapabilityPolicyError> {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                let path = format!("{parent_path}/{}", escape_json_pointer_segment(key));
                if is_protected_request_path(&path) {
                    return Err(CapabilityPolicyError::ProtectedRequestPath {
                        id: case.id.clone(),
                        path,
                    });
                }
                validate_request_patch(case, value, &path)?;
            }
        }
        Value::Array(array) => {
            for (index, value) in array.iter().enumerate() {
                let path = format!("{parent_path}/{index}");
                if is_protected_request_path(&path) {
                    return Err(CapabilityPolicyError::ProtectedRequestPath {
                        id: case.id.clone(),
                        path,
                    });
                }
                validate_request_patch(case, value, &path)?;
            }
        }
        Value::String(value) => validate_public_url(value)?,
        _ => {}
    }
    Ok(())
}

fn escape_json_pointer_segment(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

fn is_protected_request_path(path: &str) -> bool {
    PROTECTED_REQUEST_PATHS.iter().any(|protected| {
        path == *protected
            || path
                .strip_prefix(protected)
                .map(|suffix| suffix.starts_with('/'))
                .unwrap_or(false)
    })
}

fn validate_route_tags(
    configuration: &CapabilityConfiguration,
) -> Result<(), CapabilityPolicyError> {
    for assignment in &configuration.route_tags {
        if assignment.selector.tag.is_some() {
            return Err(CapabilityPolicyError::RouteTagAssignmentSelectsByTag {
                id: assignment.id.clone(),
            });
        }
        if assignment.tags.is_empty() || assignment.tags.iter().any(|tag| tag.trim().is_empty()) {
            return Err(CapabilityPolicyError::RouteTagAssignmentHasEmptyTags {
                id: assignment.id.clone(),
            });
        }
    }
    Ok(())
}

fn compile_expectations(
    configuration: &CapabilityConfiguration,
) -> Result<Vec<CompiledExpectation>, CapabilityPolicyError> {
    let bundles = configuration
        .bundles
        .iter()
        .map(|bundle| (bundle.id.as_str(), &bundle.required))
        .collect::<BTreeMap<_, _>>();

    configuration
        .compatibility_expectations
        .iter()
        .map(|expectation| compile_expectation(expectation, &bundles))
        .collect()
}

fn compile_expectation(
    expectation: &CompatibilityExpectation,
    bundles: &BTreeMap<&str, &BTreeSet<Capability>>,
) -> Result<CompiledExpectation, CapabilityPolicyError> {
    let mut required = BTreeSet::new();
    for bundle_id in &expectation.bundles {
        let bundle = bundles.get(bundle_id.as_str()).ok_or_else(|| {
            CapabilityPolicyError::UnknownCompatibilityBundle {
                expectation: expectation.id.clone(),
                bundle: bundle_id.clone(),
            }
        })?;
        required.extend(bundle.iter().copied());
    }

    Ok(CompiledExpectation {
        id: expectation.id.clone(),
        required,
        client_profiles: expectation.client_profiles.clone(),
        permitted_optional_downgrades: expectation.permitted_optional_downgrades.clone(),
        https_image_fixture: expectation.https_image_fixture.clone(),
        selector: expectation.selector.clone(),
        compiled_selector: CompiledSelector::compile(&expectation.id, &expectation.selector)?,
    })
}

fn validate_policy_conflicts(
    configuration: &CapabilityConfiguration,
    policies: &[CompiledPolicy],
) -> Result<(), CapabilityPolicyError> {
    for (offset, left) in policies.iter().enumerate() {
        for right in &policies[offset + 1..] {
            if policy_rank(configuration, left) != policy_rank(configuration, right)
                || !selectors_overlap(&left.selector, &right.selector)
            {
                continue;
            }
            let left_source = &configuration.policies[left.source_index];
            let right_source = &configuration.policies[right.source_index];
            if let Some(field) = conflicting_semantic_field(left_source, right_source) {
                return Err(CapabilityPolicyError::AmbiguousSemanticField {
                    field,
                    first: left_source.id.clone(),
                    second: right_source.id.clone(),
                });
            }
        }
    }
    Ok(())
}

fn conflicting_semantic_field(left: &CapabilityPolicy, right: &CapabilityPolicy) -> Option<String> {
    let left = &left.semantic;
    let right = &right.semantic;
    if specified_values_conflict(left.context_window, right.context_window) {
        return Some("context_window".to_owned());
    }
    if specified_values_conflict(left.max_output_tokens, right.max_output_tokens) {
        return Some("max_output_tokens".to_owned());
    }
    if specified_values_conflict(left.reasoning_mode, right.reasoning_mode) {
        return Some("reasoning_mode".to_owned());
    }
    if specified_values_conflict(
        left.reasoning_replay_required,
        right.reasoning_replay_required,
    ) {
        return Some("reasoning_replay_required".to_owned());
    }
    left.effort_map.iter().find_map(|(key, left_value)| {
        right
            .effort_map
            .get(key)
            .filter(|right_value| *right_value != left_value)
            .map(|_| format!("effort_map.{key}"))
    })
}

fn specified_values_conflict<T: Eq + Copy>(left: Option<T>, right: Option<T>) -> bool {
    matches!((left, right), (Some(left), Some(right)) if left != right)
}

fn validate_override_conflicts(
    configuration: &CapabilityConfiguration,
    route_overrides: &[CompiledOverride],
) -> Result<(), CapabilityPolicyError> {
    for (offset, left) in route_overrides.iter().enumerate() {
        for right in &route_overrides[offset + 1..] {
            if override_rank(configuration, left) != override_rank(configuration, right)
                || !selectors_overlap(&left.selector, &right.selector)
            {
                continue;
            }
            let left_source = &configuration.route_overrides[left.source_index];
            let right_source = &configuration.route_overrides[right.source_index];
            if let Some(field) = conflicting_override_field(left_source, right_source) {
                return Err(CapabilityPolicyError::AmbiguousOverrideField {
                    field,
                    first: left_source.id.clone(),
                    second: right_source.id.clone(),
                });
            }
        }
    }
    Ok(())
}

fn conflicting_override_field(
    left: &RouteCapabilityOverride,
    right: &RouteCapabilityOverride,
) -> Option<String> {
    if let Some(capability) = left
        .capabilities
        .iter()
        .find_map(|(capability, left_state)| {
            right
                .capabilities
                .get(capability)
                .filter(|right_state| *right_state != left_state)
                .map(|_| capability)
        })
    {
        return Some(format!("capabilities.{capability:?}"));
    }
    if let Some(extension) = left.extensions.iter().find_map(|(extension, left_state)| {
        right
            .extensions
            .get(extension)
            .filter(|right_state| *right_state != left_state)
            .map(|_| extension)
    }) {
        return Some(format!("extensions.{extension}"));
    }
    if specified_values_conflict(left.token_limit_field, right.token_limit_field) {
        return Some("token_limit_field".to_owned());
    }
    if specified_values_conflict(left.reasoning_carrier, right.reasoning_carrier) {
        return Some("reasoning_carrier".to_owned());
    }
    if !left.correction_rules.is_empty()
        && !right.correction_rules.is_empty()
        && left.correction_rules != right.correction_rules
    {
        return Some("correction_rules".to_owned());
    }
    None
}

fn policy_rank(configuration: &CapabilityConfiguration, policy: &CompiledPolicy) -> (i32, u8) {
    (
        configuration.policies[policy.source_index].priority,
        policy.selector.specificity,
    )
}

fn override_rank(
    configuration: &CapabilityConfiguration,
    route_override: &CompiledOverride,
) -> (i32, u8) {
    (
        configuration.route_overrides[route_override.source_index].priority,
        route_override.selector.specificity,
    )
}

fn selectors_overlap(left: &CompiledSelector, right: &CompiledSelector) -> bool {
    if !left.is_satisfiable() || !right.is_satisfiable() {
        return false;
    }
    if exact_values_differ(
        left.source.exposed_model.as_ref(),
        right.source.exposed_model.as_ref(),
    ) || exact_values_differ(
        left.source.runtime_model.as_ref(),
        right.source.runtime_model.as_ref(),
    ) || exact_values_differ(
        left.source.upstream_id.as_ref(),
        right.source.upstream_id.as_ref(),
    ) || exact_values_differ(
        left.source.protocol.as_ref(),
        right.source.protocol.as_ref(),
    ) {
        return false;
    }
    if let (Some(runtime_model), Some(glob)) = (
        left.source.runtime_model.as_deref(),
        right.runtime_model_glob.as_ref(),
    ) {
        if !glob.is_match(runtime_model) {
            return false;
        }
    }
    if let (Some(runtime_model), Some(glob)) = (
        right.source.runtime_model.as_deref(),
        left.runtime_model_glob.as_ref(),
    ) {
        if !glob.is_match(runtime_model) {
            return false;
        }
    }
    true
}

fn exact_values_differ<T: Eq>(left: Option<&T>, right: Option<&T>) -> bool {
    matches!((left, right), (Some(left), Some(right)) if left != right)
}
