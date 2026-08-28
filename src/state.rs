use crate::capabilities::{
    normalize_route_base_url, profile_is_current, route_fingerprint, Capability,
    CapabilityConfiguration, CapabilityHintKey, CapabilityRuntimeSnapshot, CapabilityStateDocument,
    DialectProfileKey, EvidenceState, ProbeConfigurationBinding, ProbeJob, ProbeJobBatch,
    ProbeMode, ProbeReason, RouteFingerprintInput, RuntimeCapabilityHintSnapshot,
    RuntimeCapabilityHints, UpstreamDialectProfile, WireProtocol, DIALECT_PROBE_SCHEMA_VERSION,
};
use crate::keys::{
    anonymous_route_id, upstream_key_fingerprint, validated_downstream_plaintext,
    verify_downstream_key,
};
use crate::routing::{
    select_upstream_with_model_matching, RouteError, RouteRequest, UpstreamCandidate,
    UpstreamProtocol,
};

#[path = "state/account_concurrency.rs"]
mod account_concurrency;
#[path = "state/calendar.rs"]
mod calendar;
#[path = "state/file_store.rs"]
mod file_store;
#[path = "state/log_queries.rs"]
pub mod log_queries;
#[path = "state/postgres.rs"]
mod postgres;

/// Test-only seam: builds an INSERT statement with one `$n` placeholder per
/// column so the placeholder list can never drift from the column list by
/// hand. Exposed for integration tests (the state module lives in the
/// gateway-core dependency crate, whose `#[cfg(test)]` items are not compiled
/// by the root package's test run).
#[doc(hidden)]
pub use postgres::insert_statement;
#[path = "state/redis_runtime.rs"]
mod redis_runtime;
#[path = "state/store.rs"]
mod store;

#[path = "state/context_profile.rs"]
mod context_profile;
#[path = "state/freekey_sync.rs"]
mod freekey_sync;
#[path = "state/model_discovery.rs"]
mod model_discovery;
#[path = "state/model_identity.rs"]
pub mod model_identity;
#[path = "state/model_key_sync.rs"]
mod model_key_sync;
#[path = "state/model_qualification.rs"]
mod model_qualification;
#[path = "state/normalize.rs"]
mod normalize;

pub use normalize::DownstreamModelEntry;
#[path = "state/route_health.rs"]
mod route_health;
#[path = "state/runtime_settings.rs"]
mod runtime_settings;
#[path = "state/types.rs"]
mod types;
#[path = "state/usage.rs"]
mod usage;

use arc_swap::ArcSwap;
use futures_util::{stream, StreamExt};
use reqwest::Client;
use serde::Serialize;
use serde_json::{Map, Value};
use sha2::Digest;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::env;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::Duration;
use subtle::ConstantTimeEq;
use tokio::fs;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, Mutex};
use uuid::Uuid;

const CODEX_SUBAGENT_MODEL_VARIANT_SUFFIX: &str = "-fast-preview";

pub(crate) fn codex_subagent_base_model(model: &str) -> Option<&str> {
    let model = model.trim();
    let base_len = model
        .len()
        .checked_sub(CODEX_SUBAGENT_MODEL_VARIANT_SUFFIX.len())?;
    let (base, suffix) = model.split_at(base_len);
    (!base.is_empty() && suffix == CODEX_SUBAGENT_MODEL_VARIANT_SUFFIX).then_some(base)
}

pub use account_concurrency::{
    AccountConcurrencyKey, AccountConcurrencyRegistry, AccountConcurrencySnapshot,
    AccountConcurrencyTuning, AccountLeaseError, AccountProbeLease, AccountProbeOutcome,
    AccountWaitTicket, ProbeDecision,
};
pub use calendar::{
    CalendarDay, CalendarError, CalendarRange, DeploymentCalendar, LogWindowMode,
    ResolvedLogWindow, SummaryRange,
};
use file_store::FileStateStore;
pub use log_queries::{DownstreamUsageSummary, EnrichedUsageLog, UsageLogPage, UsageLogQuery};
use postgres::PostgresStateStore;
pub use redis_runtime::{
    CoordinationTestFault, RuntimeCoordinationBackend, RuntimeCoordinationError,
};
pub use store::{StateStore, StoreFuture};

pub use freekey_sync::{FreekeySyncError, FreekeySyncItem, FreekeySyncSummary};
#[allow(unused_imports)]
pub use model_discovery::{
    fetch_models_from_upstream, fetch_models_from_upstream_keys_concurrently, model_discovery_url,
    KeyModelDiscoveryResult, ModelDiscoveryError,
};
pub use model_identity::{
    canonical_model_id, canonical_subagent_base_model_id, find_equivalent_stored,
    model_identity_key_with, models_equivalent, models_equivalent_with,
};
pub use model_key_sync::{
    ModelKeySyncService, ModelKeySyncSummary, TARGETED_DISCOVERY_QUEUE_CAPACITY,
};
pub use model_qualification::{
    build_key_qualification_decision, classify_qualification_level, confirmed_level,
    qualify_model_on_upstream, DirectQualificationResult, KeyQualificationDecision,
    ModelQualificationApplySummary, ModelQualificationCategory, ModelQualificationEvidence,
    ModelQualificationLevel, QualificationObservation, UpstreamQualificationDecision,
};
pub use route_health::{
    normalize_concurrency_probe_delays, HealthLease, HealthStateSnapshot, KeyHealthKey,
    RouteAvailability, RouteHealthDetailDto, RouteHealthKey, RouteHealthPermit,
    RouteHealthRegistry, RouteOutcome, RouteRecovery, RouteSetAggregateKey,
    DEFAULT_ROUTE_HEALTH_HALF_OPEN_EXCLUSIVE_WINDOW_MS, DEFAULT_ROUTE_HEALTH_HALF_OPEN_TTL_SECONDS,
    HALF_OPEN_BUSY_RETRY, ROUTE_HEALTH_GLOBAL_CAPACITY, ROUTE_HEALTH_PER_UPSTREAM_CAPACITY,
};
use runtime_settings::differing_runtime_setting_fields;
pub use runtime_settings::{
    RuntimeSettings, RuntimeSettingsDocument, RuntimeSettingsResponse, RuntimeSettingsSource,
    RuntimeSettingsUpdate, RuntimeSettingsUpdateError, RuntimeSettingsValidationError,
    IMMEDIATE_RUNTIME_SETTING_FIELDS, RESTART_RUNTIME_SETTING_FIELDS,
    RUNTIME_SETTINGS_SCHEMA_VERSION,
};
pub use types::{
    default_model_context_output_reserve, default_upstream_max_concurrency,
    default_upstream_request_quota_5h, default_upstream_request_quota_requests,
    default_upstream_request_quota_window_hours, default_upstream_requests_per_minute,
    AnnouncementConfig, AnnouncementLevel, ApiKeyModelConfig, AppConfig,
    CompatibilityUsageMetadata, DefaultModelContextConfig, DownstreamConcurrencySnapshot,
    DownstreamConfig, GlobalContextProfile, ModelConcurrencyGroup, ModelContextConfig,
    NonstandardFieldPolicy, PersistedState, RouteFailureClass, RouteHealthSnapshotDto,
    StreamDiagnostics, UpstreamConfig, UpstreamModelMapping, UpstreamMutationError, UsageLog,
    ADMIN_SESSION_TTL_SECONDS, DEFAULT_MODEL_CASE_INSENSITIVE_MATCHING,
    DEFAULT_TOOL_ARGUMENTS_STRICT, DEFAULT_TOOL_CALL_MERGE_STRICT,
    DEFAULT_UPSTREAM_ACCOUNT_QUEUE_ADAPTIVE_BUDGET_ENABLED, DEFAULT_UPSTREAM_ACCOUNT_QUEUE_ENABLED,
    DEFAULT_UPSTREAM_ACCOUNT_QUEUE_MAX_DEPTH, DEFAULT_UPSTREAM_ACCOUNT_QUEUE_MAX_WAIT_MS,
    DEFAULT_UPSTREAM_CAPACITY_FAILURE_COOLDOWN_ENABLED,
    DEFAULT_UPSTREAM_COMMON_MODE_BREAKER_THRESHOLD,
    DEFAULT_UPSTREAM_COMMON_MODE_SAME_HOST_TRANSIENT_ENABLED,
    DEFAULT_UPSTREAM_COMMON_MODE_TRANSIENT_THRESHOLD, DEFAULT_UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS,
    DEFAULT_UPSTREAM_CONCURRENCY_RECOVERY_MAX_ROUNDS,
    DEFAULT_UPSTREAM_CONCURRENCY_RECOVERY_MAX_WAIT_MS,
    DEFAULT_UPSTREAM_CONTINUATION_PIN_ESCAPE_ENABLED,
    DEFAULT_UPSTREAM_CREDENTIALS_FIRST_STRIKE_SECONDS, DEFAULT_UPSTREAM_ERROR_BODY_EXCERPT_ENABLED,
    DEFAULT_UPSTREAM_ERROR_BODY_EXCERPT_MAX_CHARS,
    DEFAULT_UPSTREAM_HEDGE_DELAY_MS,
    DEFAULT_UPSTREAM_HEDGE_ENABLED, DEFAULT_UPSTREAM_HEDGE_INTERVAL_MS,
    DEFAULT_UPSTREAM_HEDGE_MAX_EXTRA_ATTEMPTS, DEFAULT_UPSTREAM_LEASE_STALE_AFTER_MS,
    DEFAULT_UPSTREAM_LOCAL_GATE_DISTINCT_ERROR_CODE_ENABLED,
    DEFAULT_UPSTREAM_LOCAL_GATE_FAST_FAIL_ENABLED, DEFAULT_UPSTREAM_LOCAL_GATE_MAX_WAIT_MS,
    DEFAULT_UPSTREAM_LOCAL_LEASE_TTL_SECONDS, DEFAULT_UPSTREAM_RETRY_AFTER_CAP_SECONDS,
    DEFAULT_UPSTREAM_RETRY_AFTER_COOLDOWN_CAP_SECONDS,
    DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_ALIGNMENT_TRUNCATED_ENABLED,
    DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_BUDGET_ALIGNMENT_ENABLED,
    DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_ENABLED,
    DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_ROUNDS,
    DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_WAIT_MS,
    DEFAULT_UPSTREAM_ROUTE_HALF_OPEN_BUSY_MAX_ROUNDS, DEFAULT_UPSTREAM_SAME_ROUTE_RETRY_ENABLED,
    DEFAULT_UPSTREAM_SHARED_HOST_FAILURE_DOMAIN_ENABLED,
    DEFAULT_UPSTREAM_TRANSIENT_LAST_RESORT_PROBE_ENABLED,
    DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_BASE_SECONDS,
    DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_SECONDS,
    DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_STEP,
    DEFAULT_UPSTREAM_TRANSIENT_SAME_ROUTE_RETRY_ENABLED,
};
pub use usage::{
    portal_model_is_allowed, CostUsage, DailyStats, ModelStats, PerMinuteUsage, RequestQuotaUsage,
    TokenQuota,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct LegacyRouteHealthRepairReport {
    pub scanned_routes: u64,
    pub repaired_routes: u64,
}

fn first_model_key_fingerprint(upstream: &UpstreamConfig, model: &str) -> Option<String> {
    upstream
        .keys_for_model(model)
        .into_iter()
        .next()
        .map(|api_key| upstream_key_fingerprint(&upstream.id, &api_key))
}

fn upstream_protocol_from_wire(protocol: WireProtocol) -> Option<UpstreamProtocol> {
    match protocol {
        WireProtocol::ChatCompletions => Some(UpstreamProtocol::ChatCompletions),
        WireProtocol::Responses => Some(UpstreamProtocol::Responses),
        WireProtocol::Messages => None,
    }
}

fn dialect_profile_key_is_routable(routing: &PersistedState, key: &DialectProfileKey) -> bool {
    if key.key_fingerprint.is_empty() {
        return false;
    }
    let Some(protocol) = upstream_protocol_from_wire(key.protocol) else {
        return false;
    };
    let Some(upstream) = routing
        .upstreams
        .iter()
        .find(|upstream| upstream.id == key.upstream_id)
    else {
        return false;
    };
    upstream.active
        && upstream.supports_protocol(protocol)
        && upstream
            .keys_for_model(&key.runtime_model_slug)
            .iter()
            .any(|api_key| upstream_key_fingerprint(&upstream.id, api_key) == key.key_fingerprint)
}

fn capability_profile_is_current_or_retry_pending(
    profile: &UpstreamDialectProfile,
    fingerprint: &str,
    now: u64,
    refresh_interval_seconds: u64,
) -> bool {
    if let Some(next_probe_at) = profile.next_probe_at {
        return profile.configuration_fingerprint == fingerprint
            && profile.probe_schema_version == DIALECT_PROBE_SCHEMA_VERSION
            && now < next_probe_at;
    }
    profile_is_current(profile, fingerprint, now, refresh_interval_seconds)
}

use context_profile::{
    normalize_context_profile_base_url, normalize_global_context_profiles_for_storage,
};
use usage::{
    build_downstream_request_windows, build_downstream_token_windows,
    downstream_token_retention_seconds, downstream_token_retry_after_seconds,
    DownstreamRequestEvent, DownstreamTokenEvent, DOWNSTREAM_DAILY_TOKEN_WINDOW_SECONDS,
};

pub use crate::util::{
    build_upstream_http_client, encode_secret_suffix, join_upstream_url, new_id,
    prune_expired_admin_sessions, should_bypass_proxy_for_host, should_bypass_proxy_for_url,
    unix_millis, unix_seconds,
};

const RESPONSE_HISTORY_MAX_ENTRIES: usize = 2048;
const RESPONSE_HISTORY_TTL_SECONDS: u64 = 12 * 60 * 60;
const ACTIVE_REQUEST_USER_AGENT_MAX_BYTES: usize = 256;
fn fallback_stage_failure_key(
    downstream_id: &str,
    client_family: &str,
    model_slug: &str,
    upstream_id: &str,
    stage: &str,
) -> String {
    format!(
        "{}::{}::{}::{}::{}",
        downstream_id.trim().to_ascii_lowercase(),
        client_family.trim().to_ascii_lowercase(),
        model_slug.trim().to_ascii_lowercase(),
        upstream_id.trim().to_ascii_lowercase(),
        stage.trim().to_ascii_lowercase(),
    )
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResponseHistoryEntry {
    pub items: Vec<Value>,
    pub request_state: Map<String, Value>,
    pub created_at: u64,
}

#[derive(Clone)]
struct StoredResponseHistory {
    items: Vec<Value>,
    request_state: Map<String, Value>,
    created_at: u64,
}

#[derive(Default)]
struct ResponseHistoryStore {
    entries: HashMap<(String, String), StoredResponseHistory>,
    order: VecDeque<(String, String)>,
}

impl ResponseHistoryStore {
    fn evict_expired(&mut self, now: u64) {
        while let Some(history_key) = self.order.front().cloned() {
            let is_expired = self
                .entries
                .get(&history_key)
                .map(|entry| now.saturating_sub(entry.created_at) > RESPONSE_HISTORY_TTL_SECONDS)
                .unwrap_or(true);
            if !is_expired {
                break;
            }
            self.order.pop_front();
            self.entries.remove(&history_key);
        }
    }

    fn insert(
        &mut self,
        downstream_key_id: String,
        response_id: String,
        items: Vec<Value>,
        request_state: Map<String, Value>,
        created_at: u64,
        now: u64,
    ) {
        let history_key = (downstream_key_id, response_id);
        self.entries.insert(
            history_key.clone(),
            StoredResponseHistory {
                items,
                request_state,
                created_at,
            },
        );
        self.order.retain(|existing| existing != &history_key);
        self.order.push_back(history_key);
        self.evict_expired(now);
        while self.order.len() > RESPONSE_HISTORY_MAX_ENTRIES {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
    }

    fn get(
        &mut self,
        downstream_key_id: &str,
        response_id: &str,
        now: u64,
    ) -> Option<ResponseHistoryEntry> {
        self.evict_expired(now);
        self.entries
            .get(&(downstream_key_id.to_string(), response_id.to_string()))
            .map(|entry| ResponseHistoryEntry {
                items: entry.items.clone(),
                request_state: entry.request_state.clone(),
                created_at: entry.created_at,
            })
    }
}

#[derive(Clone, Debug)]
pub struct DownstreamRequestReservation {
    downstream_id: String,
    event_id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DownstreamRuntimeCounts {
    pub admitted: u32,
    pub waiting_upstream: u32,
    pub running: u32,
}

#[derive(Default)]
struct DownstreamLeaseState {
    /// admitted lease id -> absolute expiry (unix millis).  Expiry is what
    /// makes the local runtime coordination self-healing: a lease whose
    /// release was skipped (e.g. a dropped guard outside a Tokio runtime)
    /// stops consuming capacity once it lapses, instead of pinning the
    /// downstream's "running" count forever.
    admitted: HashMap<String, u64>,
    waiting: HashSet<String>,
}

impl DownstreamLeaseState {
    fn prune_expired(&mut self, now_ms: u64) {
        self.admitted.retain(|_, expires_at| *expires_at > now_ms);
        self.waiting
            .retain(|lease_id| self.admitted.contains_key(lease_id));
    }

    fn is_empty(&self) -> bool {
        self.admitted.is_empty() && self.waiting.is_empty()
    }
}

#[derive(Clone)]
pub struct DownstreamConcurrencyLease {
    downstream_id: String,
    /// Matched `model_concurrency_groups` entry name, or "" when the model
    /// matched no group (legacy single-budget bucket).
    group_name: String,
    lease_id: Option<String>,
    release_state: Arc<AtomicU8>,
}

impl std::fmt::Debug for DownstreamConcurrencyLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DownstreamConcurrencyLease")
            .field("downstream_id", &self.downstream_id)
            .field("group_name", &self.group_name)
            .field("active", &self.lease_id.is_some())
            .finish_non_exhaustive()
    }
}

impl DownstreamConcurrencyLease {
    pub fn downstream_id(&self) -> &str {
        &self.downstream_id
    }

    pub fn group_name(&self) -> &str {
        &self.group_name
    }

    /// The opaque lease identifier, when a runtime lease was actually
    /// reserved (rate limiting enabled). `None` for unrestricted leases.
    pub fn lease_id(&self) -> Option<&str> {
        self.lease_id.as_deref()
    }
}

/// Concurrency tracking key for the downstream gate. The first element is the
/// downstream id; the second is the matched `model_concurrency_groups` entry
/// name ("" = no group, the legacy single-budget bucket). Splitting the legacy
/// single key into a tuple lets a downstream key budget each model group
/// independently (C7) while keeping the no-group path byte-identical.
type DownstreamLeaseKey = (String, String);

/// All admitted-lease counts for a downstream across every group bucket.
fn downstream_total_admitted(
    runtime: &HashMap<DownstreamLeaseKey, DownstreamLeaseState>,
    downstream_id: &str,
) -> usize {
    runtime
        .iter()
        .filter(|((id, _), _)| id == downstream_id)
        .map(|(_, state)| state.admitted.len())
        .sum()
}

#[derive(Clone)]
pub struct UpstreamRequestLease {
    account: AccountConcurrencyKey,
    lease_id: String,
    release_state: Arc<AtomicU8>,
}

impl std::fmt::Debug for UpstreamRequestLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UpstreamRequestLease")
            .field("upstream_id", &self.account.upstream_id)
            .finish_non_exhaustive()
    }
}

impl UpstreamRequestLease {
    pub fn upstream_id(&self) -> &str {
        &self.account.upstream_id
    }
}

const LEASE_RELEASE_ACTIVE: u8 = 0;
const LEASE_RELEASE_RELEASING: u8 = 1;
const LEASE_RELEASED: u8 = 2;

struct LeaseReleaseGuard {
    state: Arc<AtomicU8>,
    completed: bool,
}

impl LeaseReleaseGuard {
    fn acquire(state: &Arc<AtomicU8>) -> Result<Option<Self>, RuntimeCoordinationError> {
        match state.compare_exchange(
            LEASE_RELEASE_ACTIVE,
            LEASE_RELEASE_RELEASING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => Ok(Some(Self {
                state: state.clone(),
                completed: false,
            })),
            Err(LEASE_RELEASED) => Ok(None),
            Err(_) => Err(RuntimeCoordinationError),
        }
    }

    fn complete(mut self) {
        self.state.store(LEASE_RELEASED, Ordering::Release);
        self.completed = true;
    }
}

impl Drop for LeaseReleaseGuard {
    fn drop(&mut self) {
        if !self.completed {
            self.state.store(LEASE_RELEASE_ACTIVE, Ordering::Release);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManualProbeBatchCandidateState {
    Queued,
    Reused,
    Running,
    Completed,
    Failed,
    CooldownSkipped,
    Superseded,
}

/// The terminal outcome of executing a single capability probe job.
/// `finish_capability_probe_job` maps each variant onto a batch candidate
/// state and a diagnostic evidence code so skipped, superseded and failed
/// probes are visible in the batch terminal state instead of vanishing.
#[derive(Debug)]
pub enum ProbeJobExecution {
    Completed,
    Failed(io::Error),
    CooldownSkipped { cooldown_remaining: Duration },
    Superseded,
}

#[derive(Clone, Serialize)]
pub struct ProbeCandidateSummary {
    pub upstream_id: String,
    pub route_id: String,
    pub exposed_model_slug: String,
    pub runtime_model_slug: String,
    pub protocol: WireProtocol,
    pub state: ManualProbeBatchCandidateState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic_code: Option<String>,
}

#[derive(Clone, Serialize)]
pub struct CapabilityProbeBatchStatus {
    pub batch_id: String,
    pub configuration_revision: u64,
    pub started_at: u64,
    pub queued_routes: usize,
    pub reused_routes: usize,
    /// The exposed model slugs covered by this batch (sorted, deduplicated).
    pub models: Vec<String>,
    /// Live estimate of the remaining wall-clock time, computed from the
    /// settled candidate ratio; `None` before any candidate settles.
    pub estimated_remaining_seconds: Option<u64>,
    pub candidates: Vec<ProbeCandidateSummary>,
    pub terminal_at: Option<u64>,
}

pub struct ManualProbeBatchReceipt {
    pub batch_id: String,
    pub configuration_revision: u64,
    pub started_at: u64,
    pub queued_routes: usize,
    pub reused_routes: usize,
    pub models: Vec<String>,
    pub candidates: Vec<ProbeCandidateSummary>,
}

struct CapabilityProbeBatchCandidate {
    summary: ProbeCandidateSummary,
    key: DialectProfileKey,
    binding: ProbeConfigurationBinding,
}

struct CapabilityProbeBatchRecord {
    status: CapabilityProbeBatchStatus,
    candidates: Vec<CapabilityProbeBatchCandidate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManualProbeBatchError {
    CapabilityPolicyMissing,
    CapabilityProbeDisabled,
    NoEligibleRoutes,
    QueueUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CapabilityProbeSubmission {
    Queued,
    AlreadyPending,
}

#[derive(Clone)]
pub struct AppState {
    inner: Arc<Mutex<PersistedState>>,
    config_persist_lock: Arc<Mutex<()>>,
    runtime_settings: Arc<ArcSwap<RuntimeSettings>>,
    startup_runtime_settings: Arc<RuntimeSettings>,
    capability_snapshot: Arc<ArcSwap<CapabilityRuntimeSnapshot>>,
    capability_update_lock: Arc<Mutex<()>>,
    archived_usage_logs: Arc<Mutex<Vec<UsageLog>>>,
    pending_usage_logs: Arc<Mutex<Vec<UsageLog>>>,
    usage_log_flush_running: Arc<AtomicBool>,
    upstream_runtime_state: Arc<Mutex<HashMap<String, UpstreamRuntimeState>>>,
    upstream_lease_table: Arc<StdMutex<UpstreamLeaseTable>>,
    /// C3: in-process count of requests currently waiting on the local
    /// pre-dispatch concurrency gate for a free slot, keyed by account.
    /// Bounds the queue depth (`upstream_account_queue_max_depth`).  Local
    /// backend only — the Redis backend enforces concurrency inside Lua and
    /// never produces the LocalConcurrency rejection this queue serves.
    local_slot_waiters: Arc<StdMutex<HashMap<AccountConcurrencyKey, usize>>>,
    route_health: Arc<Mutex<RouteHealthRegistry>>,
    account_concurrency: Arc<AccountConcurrencyRegistry>,
    runtime_coordination: RuntimeCoordinationBackend,
    runtime_capability_hints: Arc<StdMutex<RuntimeCapabilityHints>>,
    downstream_request_windows: Arc<Mutex<HashMap<String, VecDeque<DownstreamRequestEvent>>>>,
    downstream_token_windows: Arc<Mutex<HashMap<String, VecDeque<DownstreamTokenEvent>>>>,
    downstream_runtime: Arc<StdMutex<HashMap<DownstreamLeaseKey, DownstreamLeaseState>>>,
    active_requests: Arc<StdMutex<HashMap<String, ActiveGatewayRequest>>>,
    /// E5.1: sliding-window count of client-visible *retryable-capacity*
    /// terminal errors (code/category `upstream_routes_exhausted`,
    /// `gateway_concurrency_saturated`, or the upstream 429
    /// `upstream_rate_limited`), keyed by `(downstream_id, model)`.  A count
    /// far above the real request rate for a key is the client-retry-loop
    /// tell — the exact shape of the E-admission incidents where the gateway
    /// kept answering 429/exhausted while the client hammered it.
    retry_terminal: Arc<StdMutex<HashMap<(String, String), VecDeque<u64>>>>,
    response_history: Arc<StdMutex<ResponseHistoryStore>>,
    fallback_stage_failures: Arc<StdMutex<HashMap<String, u8>>>,
    routing_affinity: Arc<StdMutex<HashMap<String, RoutingAffinityEntry>>>,
    routing_tie_breakers: Arc<StdMutex<HashMap<String, u64>>>,
    admin_sessions: Arc<StdMutex<HashMap<String, u64>>>,
    capability_probe_sender: Arc<StdMutex<Option<mpsc::Sender<ProbeJobBatch>>>>,
    capability_probe_submissions:
        Arc<StdMutex<HashMap<DialectProfileKey, ProbeConfigurationBinding>>>,
    capability_probe_batches: Arc<StdMutex<HashMap<String, CapabilityProbeBatchRecord>>>,
    model_key_sync_runtime: Arc<StdMutex<model_key_sync::ModelKeySyncRuntime>>,
    model_key_sync_lock: Arc<Mutex<()>>,
    model_alias_registry: Arc<ArcSwap<model_identity::ModelAliasRegistry>>,
    troubleshooting_route_capture_token: Arc<str>,
    replica_owner_token: Arc<str>,
    pub store_path: PathBuf,
    pub config: AppConfig,
    deployment_calendar: DeploymentCalendar,
    client: Client,
    direct_client: Client,
    config_store: Arc<dyn StateStore>,
    postgres: Option<Arc<PostgresStateStore>>,
}

fn route_health_registry_from_config(config: &AppConfig) -> Arc<Mutex<RouteHealthRegistry>> {
    Arc::new(Mutex::new(RouteHealthRegistry::new_with_runtime_tuning(
        ROUTE_HEALTH_GLOBAL_CAPACITY,
        ROUTE_HEALTH_PER_UPSTREAM_CAPACITY,
        config.upstream_concurrency_probe_delays_ms.clone(),
        config.upstream_transient_route_cooldown_base_seconds,
        config.upstream_transient_route_cooldown_max_seconds,
        config.upstream_transient_route_cooldown_max_step,
        config.upstream_route_health_half_open_ttl_seconds,
        config.upstream_route_half_open_exclusive_window_ms,
        config.upstream_credentials_first_strike_seconds,
        config.upstream_capacity_failure_cooldown_enabled,
    )))
}

fn account_concurrency_registry_from_config(config: &AppConfig) -> Arc<AccountConcurrencyRegistry> {
    Arc::new(AccountConcurrencyRegistry::new(
        AccountConcurrencyTuning::from_config(config),
    ))
}

fn config_with_persisted_runtime_settings(
    state: &PersistedState,
    mut config: AppConfig,
) -> (AppConfig, RuntimeSettings) {
    let startup_settings = RuntimeSettings::from_app_config(&config);
    let settings = state
        .runtime_settings
        .as_ref()
        .filter(|document| document.schema_version == RUNTIME_SETTINGS_SCHEMA_VERSION)
        .and_then(|document| validated_persisted_runtime_settings(document.settings.clone()))
        .unwrap_or(startup_settings);
    settings.apply_to_app_config(&mut config);
    (config, settings)
}

/// Validate a persisted runtime-settings document, repairing a T1.1
/// cooldown-ceiling violation instead of discarding the whole document.
///
/// The all-or-nothing predecessor was a silent data-loss path on upgrade: a
/// release that tightens the cooldown-ceiling invariant makes every previously
/// saved document invalid, and dropping it reverts *every* runtime setting the
/// operator ever changed through Admin — with one error line as the only
/// evidence.  Only the cooldown-ceiling arm is repairable; any other validation
/// failure still discards, since we have no principled correction for it.
fn validated_persisted_runtime_settings(settings: RuntimeSettings) -> Option<RuntimeSettings> {
    let rejection = match settings.clone().validate_and_normalize() {
        Ok(settings) => return Some(settings),
        Err(error) => error,
    };

    let mut repaired = settings;
    let Some(corrected_max_wait_ms) = repaired.repair_cooldown_ceiling_invariant() else {
        tracing::error!(error = %rejection, "ignoring invalid persisted runtime settings");
        return None;
    };

    match repaired.validate_and_normalize() {
        Ok(settings) => {
            tracing::error!(
                auto_corrected = true,
                corrected_retry_max_wait_ms = corrected_max_wait_ms,
                rejection = %rejection,
                "持久化运行时设置违反冷却上界不变量：已自动把 upstream_route_exhaustion_retry_max_wait_ms 抬到 {corrected_max_wait_ms}ms（ceiling × 1.5）并保留其余已保存设置；请在 Admin 中下调冷却参数以消除告警"
            );
            Some(settings)
        }
        Err(error) => {
            tracing::error!(error = %error, "ignoring invalid persisted runtime settings");
            None
        }
    }
}

fn new_internal_route_capture_token() -> Arc<str> {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple()).into()
}

fn validate_downstream_plaintext_pair(downstream: &mut DownstreamConfig) {
    let has_invalid_plaintext = downstream
        .plaintext_key
        .as_deref()
        .is_some_and(|plaintext| {
            validated_downstream_plaintext(Some(plaintext), &downstream.hash).is_none()
        });
    if has_invalid_plaintext {
        tracing::warn!(
            downstream_id = %downstream.id,
            "clearing downstream plaintext that does not match its hash"
        );
        downstream.plaintext_key = None;
    }
}

fn validate_downstream_plaintext_pairs(state: &mut PersistedState) {
    for downstream in Arc::make_mut(&mut state.downstreams) {
        validate_downstream_plaintext_pair(downstream);
    }
}

fn downstream_plaintext_pairs_unchanged(
    before: &[DownstreamConfig],
    after: &[DownstreamConfig],
) -> bool {
    before.len() == after.len()
        && before.iter().zip(after).all(|(before, after)| {
            before.id == after.id
                && before.hash == after.hash
                && before.plaintext_key == after.plaintext_key
        })
}

impl StateStore for PostgresStateStore {
    fn backend_name(&self) -> &'static str {
        "postgres"
    }

    fn persist_config<'a>(&'a self, state: &'a PersistedState) -> StoreFuture<'a, io::Result<()>> {
        Box::pin(async move { self.replace_state(state).await })
    }

    fn load_capability_state<'a>(&'a self) -> StoreFuture<'a, io::Result<CapabilityStateDocument>> {
        Box::pin(async move { PostgresStateStore::load_capability_state(self).await })
    }

    fn persist_capability_configuration<'a>(
        &'a self,
        config: &'a CapabilityConfiguration,
    ) -> StoreFuture<'a, io::Result<()>> {
        Box::pin(
            async move { PostgresStateStore::persist_capability_configuration(self, config).await },
        )
    }

    fn upsert_dialect_profile<'a>(
        &'a self,
        profile: &'a UpstreamDialectProfile,
    ) -> StoreFuture<'a, io::Result<()>> {
        Box::pin(async move { PostgresStateStore::upsert_dialect_profile(self, profile).await })
    }

    fn delete_dialect_profiles_for_upstream<'a>(
        &'a self,
        upstream_id: &'a str,
    ) -> StoreFuture<'a, io::Result<()>> {
        Box::pin(async move {
            PostgresStateStore::delete_dialect_profiles_for_upstream(self, upstream_id).await
        })
    }

    fn delete_dialect_profile<'a>(
        &'a self,
        key: &'a DialectProfileKey,
    ) -> StoreFuture<'a, io::Result<()>> {
        Box::pin(async move { PostgresStateStore::delete_dialect_profile(self, key).await })
    }

    fn query_usage_logs_page<'a>(
        &'a self,
        query: &'a UsageLogQuery,
    ) -> StoreFuture<'a, io::Result<Option<UsageLogPage>>> {
        Box::pin(async move { self.query_usage_logs_page(query).await })
    }

    fn query_usage_logs_window<'a>(
        &'a self,
        start_time: u64,
        end_time: u64,
    ) -> StoreFuture<'a, io::Result<Option<Vec<UsageLog>>>> {
        Box::pin(async move { self.query_usage_logs_window(start_time, end_time).await })
    }

    fn downstream_usage_summary<'a>(
        &'a self,
        downstream_id: &'a str,
    ) -> StoreFuture<'a, io::Result<Option<DownstreamUsageSummary>>> {
        Box::pin(async move { self.downstream_usage_summary(downstream_id).await })
    }

    fn downstream_daily_stats<'a>(
        &'a self,
        downstream_id: &'a str,
        calendar: &'a CalendarRange,
    ) -> StoreFuture<'a, io::Result<Option<Vec<DailyStats>>>> {
        Box::pin(async move { self.downstream_daily_stats(downstream_id, calendar).await })
    }

    fn delete_usage_logs_before<'a>(&'a self, cutoff: u64) -> StoreFuture<'a, io::Result<()>> {
        Box::pin(async move { self.delete_usage_logs_before(cutoff).await })
    }
}

impl AppState {
    pub fn new(state: PersistedState, store_path: impl Into<PathBuf>, config: AppConfig) -> Self {
        let deployment_calendar = DeploymentCalendar::parse(&config.deployment_timezone)
            .expect("deployment timezone must be valid before state construction");
        Self::new_with_archived(
            state,
            Vec::new(),
            store_path,
            config,
            RuntimeCoordinationBackend::Local,
            deployment_calendar,
        )
    }

    pub fn store_response_history(
        &self,
        downstream_key_id: impl Into<String>,
        response_id: impl Into<String>,
        items: Vec<Value>,
        request_state: Map<String, Value>,
    ) {
        let downstream_key_id = downstream_key_id.into();
        let response_id = response_id.into();
        let created_at = unix_seconds();
        {
            let mut history = self.response_history.lock().unwrap();
            history.insert(
                downstream_key_id.clone(),
                response_id.clone(),
                items.clone(),
                request_state.clone(),
                created_at,
                created_at,
            );
        }

        let Some(postgres) = self.postgres.clone() else {
            return;
        };
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                if let Err(error) = postgres
                    .upsert_response_history(
                        &downstream_key_id,
                        &response_id,
                        &items,
                        &request_state,
                        created_at,
                    )
                    .await
                {
                    tracing::warn!(
                        downstream_key_id = %downstream_key_id,
                        response_id = %response_id,
                        error = %error,
                        "failed to persist response history"
                    );
                }
            });
        }
    }

    pub async fn response_history(
        &self,
        downstream_key_id: &str,
        response_id: &str,
    ) -> Option<ResponseHistoryEntry> {
        let now = unix_seconds();
        {
            let mut history = self.response_history.lock().unwrap();
            if let Some(entry) = history.get(downstream_key_id, response_id, now) {
                return Some(entry);
            }
        }

        let postgres = self.postgres.clone()?;
        let allow_legacy_claim = {
            let state = self.inner.lock().await;
            state.downstreams.len() == 1
                && state
                    .downstreams
                    .first()
                    .is_some_and(|downstream| downstream.id == downstream_key_id)
        };
        let entry = postgres
            .response_history(
                downstream_key_id,
                response_id,
                now.saturating_sub(RESPONSE_HISTORY_TTL_SECONDS),
                allow_legacy_claim,
            )
            .await
            .ok()
            .flatten()?;

        let mut history = self.response_history.lock().unwrap();
        history.insert(
            downstream_key_id.to_string(),
            response_id.to_string(),
            entry.items.clone(),
            entry.request_state.clone(),
            entry.created_at,
            now,
        );
        Some(entry)
    }

    pub fn fallback_stage_failure_count(
        &self,
        downstream_id: &str,
        client_family: &str,
        model_slug: &str,
        upstream_id: &str,
        stage: &str,
    ) -> u8 {
        let key = fallback_stage_failure_key(
            downstream_id,
            client_family,
            model_slug,
            upstream_id,
            stage,
        );
        self.fallback_stage_failures
            .lock()
            .expect("fallback stage failure lock poisoned")
            .get(&key)
            .copied()
            .unwrap_or_default()
    }

    pub fn record_fallback_stage_failure(
        &self,
        downstream_id: &str,
        client_family: &str,
        model_slug: &str,
        upstream_id: &str,
        stage: &str,
    ) {
        let key = fallback_stage_failure_key(
            downstream_id,
            client_family,
            model_slug,
            upstream_id,
            stage,
        );
        let mut failures = self
            .fallback_stage_failures
            .lock()
            .expect("fallback stage failure lock poisoned");
        let entry = failures.entry(key).or_insert(0);
        *entry = entry.saturating_add(1).min(3);
    }

    pub fn clear_fallback_stage_failures(
        &self,
        downstream_id: &str,
        client_family: &str,
        model_slug: &str,
        upstream_id: &str,
    ) {
        let prefix = format!(
            "{}::{}::{}::{}::",
            downstream_id.trim().to_ascii_lowercase(),
            client_family.trim().to_ascii_lowercase(),
            model_slug.trim().to_ascii_lowercase(),
            upstream_id.trim().to_ascii_lowercase(),
        );
        self.fallback_stage_failures
            .lock()
            .expect("fallback stage failure lock poisoned")
            .retain(|key, _| !key.starts_with(&prefix));
    }

    pub fn new_with_store(
        state: PersistedState,
        store_path: impl Into<PathBuf>,
        config: AppConfig,
        config_store: Arc<dyn StateStore>,
    ) -> Self {
        let deployment_calendar = DeploymentCalendar::parse(&config.deployment_timezone)
            .expect("deployment timezone must be valid before state construction");
        Self::new_with_archived_and_store(
            state,
            Vec::new(),
            store_path.into(),
            config,
            config_store,
            None,
            deployment_calendar,
        )
    }

    fn new_with_archived(
        mut state: PersistedState,
        archived_usage_logs: Vec<UsageLog>,
        store_path: impl Into<PathBuf>,
        config: AppConfig,
        runtime_coordination: RuntimeCoordinationBackend,
        deployment_calendar: DeploymentCalendar,
    ) -> Self {
        let (config, runtime_settings) = config_with_persisted_runtime_settings(&state, config);
        for upstream in Arc::make_mut(&mut state.upstreams) {
            upstream.normalize_for_storage();
        }
        validate_downstream_plaintext_pairs(&mut state);
        state.global_context_profiles = Arc::new(normalize_global_context_profiles_for_storage(
            std::mem::take(Arc::make_mut(&mut state.global_context_profiles)),
        ));
        let model_alias_registry =
            model_identity::ModelAliasRegistry::from_rules(state.model_aliases.clone())
                .unwrap_or_else(|error| {
                    tracing::error!(error = %error, "invalid model alias rules, ignoring");
                    model_identity::ModelAliasRegistry::default()
                });
        let downstream_usage_logs = state
            .usage_logs
            .iter()
            .cloned()
            .chain(archived_usage_logs.iter().cloned())
            .collect::<Vec<_>>();
        let downstreams = state.downstreams.clone();
        let store_path = store_path.into();
        let config_store: Arc<dyn StateStore> = Arc::new(FileStateStore::new(store_path.clone()));
        Self {
            inner: Arc::new(Mutex::new(state)),
            config_persist_lock: Arc::new(Mutex::new(())),
            runtime_settings: Arc::new(ArcSwap::from_pointee(runtime_settings.clone())),
            startup_runtime_settings: Arc::new(runtime_settings),
            capability_snapshot: Arc::new(ArcSwap::from_pointee(
                CapabilityRuntimeSnapshot::default(),
            )),
            capability_update_lock: Arc::new(Mutex::new(())),
            archived_usage_logs: Arc::new(Mutex::new(archived_usage_logs)),
            pending_usage_logs: Arc::new(Mutex::new(Vec::new())),
            usage_log_flush_running: Arc::new(AtomicBool::new(false)),
            upstream_runtime_state: Arc::new(Mutex::new(HashMap::new())),
            upstream_lease_table: Arc::new(StdMutex::new(UpstreamLeaseTable::default())),
            local_slot_waiters: Arc::new(StdMutex::new(HashMap::new())),
            route_health: route_health_registry_from_config(&config),
            account_concurrency: account_concurrency_registry_from_config(&config),
            runtime_coordination,
            runtime_capability_hints: Arc::new(StdMutex::new(RuntimeCapabilityHints::default())),
            downstream_request_windows: Arc::new(Mutex::new(build_downstream_request_windows(
                &downstream_usage_logs,
            ))),
            downstream_token_windows: Arc::new(Mutex::new(build_downstream_token_windows(
                &downstream_usage_logs,
                &downstreams,
            ))),
            downstream_runtime: Arc::new(StdMutex::new(HashMap::new())),
            active_requests: Arc::new(StdMutex::new(HashMap::new())),
            retry_terminal: Arc::new(StdMutex::new(HashMap::new())),
            response_history: Arc::new(StdMutex::new(ResponseHistoryStore::default())),
            fallback_stage_failures: Arc::new(StdMutex::new(HashMap::new())),
            routing_affinity: Arc::new(StdMutex::new(HashMap::new())),
            routing_tie_breakers: Arc::new(StdMutex::new(HashMap::new())),
            admin_sessions: Arc::new(StdMutex::new(HashMap::new())),
            capability_probe_sender: Arc::new(StdMutex::new(None)),
            capability_probe_submissions: Arc::new(StdMutex::new(HashMap::new())),
            capability_probe_batches: Arc::new(StdMutex::new(HashMap::new())),
            model_key_sync_runtime: Arc::new(StdMutex::new(
                model_key_sync::ModelKeySyncRuntime::default(),
            )),
            model_key_sync_lock: Arc::new(Mutex::new(())),
            model_alias_registry: Arc::new(ArcSwap::from_pointee(model_alias_registry)),
            troubleshooting_route_capture_token: new_internal_route_capture_token(),
            replica_owner_token: new_internal_route_capture_token(),
            store_path,
            client: build_upstream_http_client(&config, false),
            direct_client: build_upstream_http_client(&config, true),
            config,
            deployment_calendar,
            config_store,
            postgres: None,
        }
    }

    fn new_with_archived_and_store(
        mut state: PersistedState,
        archived_usage_logs: Vec<UsageLog>,
        store_path: PathBuf,
        config: AppConfig,
        config_store: Arc<dyn StateStore>,
        postgres: Option<Arc<PostgresStateStore>>,
        deployment_calendar: DeploymentCalendar,
    ) -> Self {
        let (config, runtime_settings) = config_with_persisted_runtime_settings(&state, config);
        for upstream in Arc::make_mut(&mut state.upstreams) {
            upstream.normalize_for_storage();
        }
        validate_downstream_plaintext_pairs(&mut state);
        state.global_context_profiles = Arc::new(normalize_global_context_profiles_for_storage(
            std::mem::take(Arc::make_mut(&mut state.global_context_profiles)),
        ));
        let model_alias_registry =
            model_identity::ModelAliasRegistry::from_rules(state.model_aliases.clone())
                .unwrap_or_else(|error| {
                    tracing::error!(error = %error, "invalid model alias rules, ignoring");
                    model_identity::ModelAliasRegistry::default()
                });
        let downstream_usage_logs = state
            .usage_logs
            .iter()
            .cloned()
            .chain(archived_usage_logs.iter().cloned())
            .collect::<Vec<_>>();
        let downstreams = state.downstreams.clone();
        Self {
            inner: Arc::new(Mutex::new(state)),
            config_persist_lock: Arc::new(Mutex::new(())),
            runtime_settings: Arc::new(ArcSwap::from_pointee(runtime_settings.clone())),
            startup_runtime_settings: Arc::new(runtime_settings),
            capability_snapshot: Arc::new(ArcSwap::from_pointee(
                CapabilityRuntimeSnapshot::default(),
            )),
            capability_update_lock: Arc::new(Mutex::new(())),
            archived_usage_logs: Arc::new(Mutex::new(archived_usage_logs)),
            pending_usage_logs: Arc::new(Mutex::new(Vec::new())),
            usage_log_flush_running: Arc::new(AtomicBool::new(false)),
            upstream_runtime_state: Arc::new(Mutex::new(HashMap::new())),
            upstream_lease_table: Arc::new(StdMutex::new(UpstreamLeaseTable::default())),
            local_slot_waiters: Arc::new(StdMutex::new(HashMap::new())),
            route_health: route_health_registry_from_config(&config),
            account_concurrency: account_concurrency_registry_from_config(&config),
            runtime_coordination: RuntimeCoordinationBackend::Local,
            runtime_capability_hints: Arc::new(StdMutex::new(RuntimeCapabilityHints::default())),
            downstream_request_windows: Arc::new(Mutex::new(build_downstream_request_windows(
                &downstream_usage_logs,
            ))),
            downstream_token_windows: Arc::new(Mutex::new(build_downstream_token_windows(
                &downstream_usage_logs,
                &downstreams,
            ))),
            downstream_runtime: Arc::new(StdMutex::new(HashMap::new())),
            active_requests: Arc::new(StdMutex::new(HashMap::new())),
            retry_terminal: Arc::new(StdMutex::new(HashMap::new())),
            response_history: Arc::new(StdMutex::new(ResponseHistoryStore::default())),
            fallback_stage_failures: Arc::new(StdMutex::new(HashMap::new())),
            routing_affinity: Arc::new(StdMutex::new(HashMap::new())),
            routing_tie_breakers: Arc::new(StdMutex::new(HashMap::new())),
            admin_sessions: Arc::new(StdMutex::new(HashMap::new())),
            capability_probe_sender: Arc::new(StdMutex::new(None)),
            capability_probe_submissions: Arc::new(StdMutex::new(HashMap::new())),
            capability_probe_batches: Arc::new(StdMutex::new(HashMap::new())),
            model_key_sync_runtime: Arc::new(StdMutex::new(
                model_key_sync::ModelKeySyncRuntime::default(),
            )),
            model_key_sync_lock: Arc::new(Mutex::new(())),
            model_alias_registry: Arc::new(ArcSwap::from_pointee(model_alias_registry)),
            troubleshooting_route_capture_token: new_internal_route_capture_token(),
            replica_owner_token: new_internal_route_capture_token(),
            store_path,
            client: build_upstream_http_client(&config, false),
            direct_client: build_upstream_http_client(&config, true),
            config,
            deployment_calendar,
            config_store,
            postgres,
        }
    }

    async fn new_with_postgres(
        mut state: PersistedState,
        config: AppConfig,
        postgres: PostgresStateStore,
        runtime_coordination: RuntimeCoordinationBackend,
        deployment_calendar: DeploymentCalendar,
    ) -> Self {
        let (config, runtime_settings) = config_with_persisted_runtime_settings(&state, config);
        for upstream in Arc::make_mut(&mut state.upstreams) {
            upstream.normalize_for_storage();
        }
        validate_downstream_plaintext_pairs(&mut state);
        state.global_context_profiles = Arc::new(normalize_global_context_profiles_for_storage(
            std::mem::take(Arc::make_mut(&mut state.global_context_profiles)),
        ));
        let model_alias_registry =
            model_identity::ModelAliasRegistry::from_rules(state.model_aliases.clone())
                .unwrap_or_else(|error| {
                    tracing::error!(error = %error, "invalid model alias rules, ignoring");
                    model_identity::ModelAliasRegistry::default()
                });
        let downstream_usage_logs = state.usage_logs.clone();
        let downstreams = state.downstreams.clone();
        let postgres = Arc::new(postgres);
        let config_store: Arc<dyn StateStore> = postgres.clone();
        Self {
            inner: Arc::new(Mutex::new(state)),
            config_persist_lock: Arc::new(Mutex::new(())),
            runtime_settings: Arc::new(ArcSwap::from_pointee(runtime_settings.clone())),
            startup_runtime_settings: Arc::new(runtime_settings),
            capability_snapshot: Arc::new(ArcSwap::from_pointee(
                CapabilityRuntimeSnapshot::default(),
            )),
            capability_update_lock: Arc::new(Mutex::new(())),
            archived_usage_logs: Arc::new(Mutex::new(Vec::new())),
            pending_usage_logs: Arc::new(Mutex::new(Vec::new())),
            usage_log_flush_running: Arc::new(AtomicBool::new(false)),
            upstream_runtime_state: Arc::new(Mutex::new(HashMap::new())),
            upstream_lease_table: Arc::new(StdMutex::new(UpstreamLeaseTable::default())),
            local_slot_waiters: Arc::new(StdMutex::new(HashMap::new())),
            route_health: route_health_registry_from_config(&config),
            account_concurrency: account_concurrency_registry_from_config(&config),
            runtime_coordination,
            runtime_capability_hints: Arc::new(StdMutex::new(RuntimeCapabilityHints::default())),
            downstream_request_windows: Arc::new(Mutex::new(build_downstream_request_windows(
                &downstream_usage_logs,
            ))),
            downstream_token_windows: Arc::new(Mutex::new(build_downstream_token_windows(
                &downstream_usage_logs,
                &downstreams,
            ))),
            downstream_runtime: Arc::new(StdMutex::new(HashMap::new())),
            active_requests: Arc::new(StdMutex::new(HashMap::new())),
            retry_terminal: Arc::new(StdMutex::new(HashMap::new())),
            response_history: Arc::new(StdMutex::new(ResponseHistoryStore::default())),
            fallback_stage_failures: Arc::new(StdMutex::new(HashMap::new())),
            routing_affinity: Arc::new(StdMutex::new(HashMap::new())),
            routing_tie_breakers: Arc::new(StdMutex::new(HashMap::new())),
            admin_sessions: Arc::new(StdMutex::new(HashMap::new())),
            capability_probe_sender: Arc::new(StdMutex::new(None)),
            capability_probe_submissions: Arc::new(StdMutex::new(HashMap::new())),
            capability_probe_batches: Arc::new(StdMutex::new(HashMap::new())),
            model_key_sync_runtime: Arc::new(StdMutex::new(
                model_key_sync::ModelKeySyncRuntime::default(),
            )),
            model_key_sync_lock: Arc::new(Mutex::new(())),
            model_alias_registry: Arc::new(ArcSwap::from_pointee(model_alias_registry)),
            troubleshooting_route_capture_token: new_internal_route_capture_token(),
            replica_owner_token: new_internal_route_capture_token(),
            store_path: PathBuf::new(),
            client: build_upstream_http_client(&config, false),
            direct_client: build_upstream_http_client(&config, true),
            config,
            deployment_calendar,
            config_store,
            postgres: Some(postgres),
        }
    }

    pub fn troubleshooting_route_capture_token(&self) -> &str {
        &self.troubleshooting_route_capture_token
    }

    /// Stable owner token for this replica, used to acquire poller leases.
    pub fn replica_owner_token(&self) -> &str {
        &self.replica_owner_token
    }

    pub fn deployment_calendar(&self) -> &DeploymentCalendar {
        &self.deployment_calendar
    }

    pub fn account_concurrency_registry(&self) -> Arc<AccountConcurrencyRegistry> {
        self.account_concurrency.clone()
    }

    /// C3: number of real leases currently held for an account (local
    /// backend).  The Redis backend enforces concurrency inside Lua and never
    /// produces the LocalConcurrency rejection this value serves, so callers
    /// only reach it from the local-gate path.
    pub fn local_account_lease_count(&self, account: &AccountConcurrencyKey) -> usize {
        let table = self
            .upstream_lease_table
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        table.account_lease_count(account)
    }

    /// C4.2: number of leases for `account` whose last heartbeat is older than
    /// `stale_after` — i.e. leases the lazy stale sweep (C2.3) would reclaim on
    /// its next pass.  Surfaces "the gate is full because of stale/leaked
    /// leases" as opposed to "the gate is full because of genuine in-flight
    /// traffic"; both feed the fast-fail error's `stale_lease_count` detail.
    pub fn local_account_stale_lease_count(
        &self,
        account: &AccountConcurrencyKey,
        stale_after: Duration,
    ) -> usize {
        let table = self
            .upstream_lease_table
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        table.account_stale_lease_count(account, stale_after, tokio::time::Instant::now())
    }

    /// C3: atomically reserve a queue slot on the local concurrency gate for
    /// `account`, failing without side effects once the queue already holds
    /// `max_depth` waiters (bounded queue, `upstream_account_queue_max_depth`).
    pub fn try_enter_local_slot_wait(
        &self,
        account: &AccountConcurrencyKey,
        max_depth: usize,
    ) -> bool {
        let mut counts = self
            .local_slot_waiters
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let entry = counts.entry(account.clone()).or_insert(0);
        if *entry >= max_depth.max(1) {
            return false;
        }
        *entry += 1;
        true
    }

    /// C3: release a queue slot previously taken by [`Self::try_enter_local_slot_wait`].
    pub fn leave_local_slot_wait(&self, account: &AccountConcurrencyKey) {
        let mut counts = self
            .local_slot_waiters
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entry) = counts.get_mut(account) {
            *entry = entry.saturating_sub(1);
            if *entry == 0 {
                counts.remove(account);
            }
        }
    }

    /// C3: current number of requests waiting on `account`'s local
    /// concurrency slots (for the C4.2 `queue_depth` detail and, when needed,
    /// depth checks before entering the wait).
    pub fn local_slot_waiter_count(&self, account: &AccountConcurrencyKey) -> usize {
        let counts = self
            .local_slot_waiters
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        counts.get(account).copied().unwrap_or(0)
    }

    /// E4.2 (§3.4 / §3.5): the C3 local-slot queue must only be entered when
    /// the evidence says a slot can free within the wait budget.  Returns the
    /// effective queue budget in ms and whether the queue is worth entering:
    ///
    /// * budget = `clamp(p95_hold × ADAPTIVE_QUEUE_BUDGET_FACTOR,
    ///   floor = upstream_account_queue_max_wait_ms,
    ///   ceiling = ADAPTIVE_QUEUE_BUDGET_CEILING_MS)`; falls back to the
    ///   static floor when fewer than two hold samples exist.
    /// * `should_skip` = the *static* `upstream_account_queue_max_wait_ms`
    ///   floor is the only meaningful yardstick for "waiting is doomed":
    ///   comparing `p50_hold` against the p95-derived budget can never fire
    ///   (`budget ≥ p95 ≥ p50`), so the median observed hold outlasting the
    ///   operator's configured wait limit is the signal that the queue's
    ///   promise ("wait a little and a slot frees") is broken — the median
    ///   request takes longer than the whole wait.  Skipping straight to the
    ///   fast-fail removes the "10s silent wait for a doomed request" (E4's
    ///   §2.4 fix) and cheaply hands the retry loop back to the client.
    pub fn local_slot_queue_plan(&self, account: &AccountConcurrencyKey) -> (u64, bool) {
        let config = &self.config;
        let table = self
            .upstream_lease_table
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let floor_ms = config.upstream_account_queue_max_wait_ms.max(1);
        let Some(p95) = table.hold_p95_seconds(account) else {
            return (floor_ms, false);
        };
        let p50 = table.hold_p50_seconds(account).unwrap_or(0);
        let scaled = (p95 as f64 * ADAPTIVE_QUEUE_BUDGET_FACTOR) as u64;
        let budget = scaled.clamp(floor_ms, ADAPTIVE_QUEUE_BUDGET_CEILING_MS);
        let should_skip = p50 > 0 && p50 > floor_ms.div_ceil(1000);
        (budget, should_skip)
    }

    /// C5.1: total C3 slot-queue depth across all of `upstream_id`'s accounts
    /// (the queue is keyed per account, the admin snapshot is per upstream).
    pub fn upstream_slot_waiter_count(&self, upstream_id: &str) -> usize {
        let counts = self
            .local_slot_waiters
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        counts
            .iter()
            .filter(|(account, _)| account.upstream_id == upstream_id)
            .map(|(_, count)| count)
            .sum()
    }

    pub async fn observe_account_concurrency(
        &self,
        account: &AccountConcurrencyKey,
        retry_after: Option<Duration>,
    ) -> Result<(), RuntimeCoordinationError> {
        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            return coordinator
                .reject_account_concurrency(account, retry_after)
                .await;
        }
        self.account_concurrency
            .reject(account, retry_after, tokio::time::Instant::now());
        Ok(())
    }

    pub async fn register_account_waiter(
        &self,
        account: &AccountConcurrencyKey,
        request_id: &str,
        downstream_id: &str,
        downstream_lease_id: &str,
    ) -> Result<AccountWaitTicket, RuntimeCoordinationError> {
        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            return coordinator
                .register_account_waiter(account, request_id, downstream_id, downstream_lease_id)
                .await;
        }
        Ok(self.account_concurrency.register_waiter(
            account.clone(),
            request_id,
            downstream_id,
            downstream_lease_id,
            tokio::time::Instant::now(),
        ))
    }

    pub async fn register_account_waiter_if_saturated(
        &self,
        account: &AccountConcurrencyKey,
        request_id: &str,
        downstream_id: &str,
        downstream_lease_id: &str,
    ) -> Result<Option<AccountWaitTicket>, RuntimeCoordinationError> {
        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            return coordinator
                .register_account_waiter_if_saturated(
                    account,
                    request_id,
                    downstream_id,
                    downstream_lease_id,
                )
                .await;
        }
        Ok(self.account_concurrency.register_waiter_if_saturated(
            account.clone(),
            request_id,
            downstream_id,
            downstream_lease_id,
            tokio::time::Instant::now(),
        ))
    }

    pub async fn register_account_waiter_for_downstream_lease_if_saturated(
        &self,
        account: &AccountConcurrencyKey,
        request_id: &str,
        lease: &DownstreamConcurrencyLease,
    ) -> Result<Option<AccountWaitTicket>, RuntimeCoordinationError> {
        self.register_account_waiter_if_saturated(
            account,
            request_id,
            &lease.downstream_id,
            lease.lease_id.as_deref().unwrap_or_default(),
        )
        .await
    }

    pub async fn account_requires_recovery(
        &self,
        account: &AccountConcurrencyKey,
    ) -> Result<bool, RuntimeCoordinationError> {
        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            return coordinator.account_requires_recovery(account).await;
        }
        Ok(self
            .account_concurrency
            .snapshot(account, tokio::time::Instant::now())
            .saturated)
    }

    pub async fn account_recovery_retry_after(
        &self,
        account: &AccountConcurrencyKey,
    ) -> Result<Duration, RuntimeCoordinationError> {
        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            return coordinator.account_recovery_retry_after(account).await;
        }
        Ok(self
            .account_concurrency
            .snapshot(account, tokio::time::Instant::now())
            .retry_after)
    }

    pub async fn renew_account_waiter(
        &self,
        ticket: &AccountWaitTicket,
    ) -> Result<(), RuntimeCoordinationError> {
        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            return coordinator.renew_account_waiter(ticket).await;
        }
        self.account_concurrency
            .renew_waiter(ticket, tokio::time::Instant::now())
            .map_err(|_| RuntimeCoordinationError)
    }

    pub async fn cancel_account_waiter(
        &self,
        ticket: &AccountWaitTicket,
    ) -> Result<(), RuntimeCoordinationError> {
        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            return coordinator.cancel_account_waiter(ticket).await;
        }
        self.account_concurrency.cancel_waiter(ticket);
        Ok(())
    }

    pub async fn try_acquire_account_probe(
        &self,
        ticket: &AccountWaitTicket,
    ) -> Result<ProbeDecision, RuntimeCoordinationError> {
        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            return coordinator.try_acquire_account_probe(ticket).await;
        }
        Ok(self
            .account_concurrency
            .try_probe(ticket, tokio::time::Instant::now()))
    }

    pub async fn try_acquire_account_probe_for_downstream_lease(
        &self,
        ticket: &AccountWaitTicket,
        lease: &DownstreamConcurrencyLease,
    ) -> Result<ProbeDecision, RuntimeCoordinationError> {
        if ticket.downstream_id != lease.downstream_id {
            return Err(RuntimeCoordinationError);
        }
        let Some(lease_id) = lease.lease_id.as_deref() else {
            return self.try_acquire_account_probe(ticket).await;
        };
        if ticket.downstream_lease_id != lease_id {
            return Err(RuntimeCoordinationError);
        }
        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            return coordinator
                .try_acquire_account_probe_for_downstream_lease(
                    ticket,
                    &lease.downstream_id,
                    lease_id,
                    &lease.group_name,
                )
                .await;
        }

        let mut runtime = self
            .downstream_runtime
            .lock()
            .expect("downstream runtime lock poisoned");
        let key = (lease.downstream_id.clone(), lease.group_name.clone());
        let downstream = runtime.get_mut(&key).ok_or(RuntimeCoordinationError)?;
        if !downstream.admitted.contains_key(lease_id) || !downstream.waiting.contains(lease_id) {
            return Err(RuntimeCoordinationError);
        }
        let decision = self
            .account_concurrency
            .try_probe(ticket, tokio::time::Instant::now());
        if matches!(decision, ProbeDecision::Granted(_)) {
            downstream.waiting.remove(lease_id);
        }
        Ok(decision)
    }

    pub async fn renew_account_probe(
        &self,
        lease: &AccountProbeLease,
    ) -> Result<(), RuntimeCoordinationError> {
        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            return coordinator.renew_account_probe(lease).await;
        }
        self.account_concurrency
            .renew_probe(lease, tokio::time::Instant::now())
            .map_err(|_| RuntimeCoordinationError)
    }

    pub async fn finish_account_probe(
        &self,
        lease: &AccountProbeLease,
        outcome: AccountProbeOutcome,
    ) -> Result<(), RuntimeCoordinationError> {
        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            return coordinator.finish_account_probe(lease, outcome).await;
        }
        self.account_concurrency
            .finish_probe(lease.clone(), outcome, tokio::time::Instant::now())
            .map_err(|_| RuntimeCoordinationError)
    }

    pub fn client(&self) -> Client {
        self.client.clone()
    }

    pub async fn runtime_coordination_healthcheck(&self) -> io::Result<()> {
        self.runtime_coordination.healthcheck().await
    }

    /// Whether the runtime coordination backend is Redis (vs the local
    /// in-memory backend).  C1.2 uses this to decide whether an upstream
    /// lease can be released synchronously from `Drop`: the local backend's
    /// lease table sits behind a plain `std::sync::Mutex`, so no Tokio
    /// runtime is needed; the Redis backend must go through the coordinator.
    pub fn is_redis_runtime_backend(&self) -> bool {
        self.runtime_coordination.is_redis()
    }

    pub async fn repair_legacy_local_admission_route_health(
        &self,
    ) -> io::Result<LegacyRouteHealthRepairReport> {
        match &self.runtime_coordination {
            RuntimeCoordinationBackend::Local => Ok(LegacyRouteHealthRepairReport {
                scanned_routes: 0,
                repaired_routes: 0,
            }),
            RuntimeCoordinationBackend::Redis(coordinator) => coordinator
                .repair_legacy_local_admission_route_health()
                .await
                .map_err(io::Error::other),
        }
    }

    async fn repair_legacy_local_admission_route_health_at_startup(&self) -> io::Result<()> {
        let report = self.repair_legacy_local_admission_route_health().await?;
        tracing::info!(
            redis_prefix = %self.config.redis_key_prefix,
            scanned_routes = report.scanned_routes,
            repaired_routes = report.repaired_routes,
            "legacy local-admission route-health repair complete"
        );
        Ok(())
    }

    /// Test-only seam: returns the coordination fault injector when the
    /// state uses the Redis runtime backend, so an integration test can arm
    /// a simulated outage or lost response without pausing the shared Redis.
    #[doc(hidden)]
    pub fn coordination_test_fault(&self) -> Option<std::sync::Arc<CoordinationTestFault>> {
        match &self.runtime_coordination {
            RuntimeCoordinationBackend::Redis(coordinator) => {
                Some(coordinator.coordination_fault.clone())
            }
            RuntimeCoordinationBackend::Local => None,
        }
    }

    pub fn client_for_url(&self, url: &str) -> Client {
        if should_bypass_proxy_for_url(url) {
            self.direct_client.clone()
        } else {
            self.client.clone()
        }
    }

    /// Reserve a single-flight half-open lease for an *early* probe of a
    /// cooling route, ignoring the remaining cooldown (A3 last-resort probe).
    /// Backed by the Redis `route_health_probe` script when runtime
    /// coordination is Redis-backed and by the local registry otherwise; both
    /// backends behave identically.
    pub async fn reserve_route_health_probe(
        &self,
        route: &RouteHealthKey,
        key: &KeyHealthKey,
    ) -> Result<RouteAvailability<RouteHealthPermit>, RuntimeCoordinationError> {
        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            let availability = coordinator
                .reserve_route_health_probe(route, key, &Uuid::new_v4().to_string())
                .await?;
            return Ok(match availability {
                RouteAvailability::Ready(lease) => RouteAvailability::Ready(
                    RouteHealthPermit::new_redis(self.clone(), coordinator.clone(), lease),
                ),
                RouteAvailability::Cooling {
                    class,
                    retry_after,
                    upstream_status,
                } => RouteAvailability::Cooling {
                    class,
                    retry_after,
                    upstream_status,
                },
                RouteAvailability::HalfOpenBusy {
                    class,
                    retry_after,
                    upstream_status,
                } => RouteAvailability::HalfOpenBusy {
                    class,
                    retry_after,
                    upstream_status,
                },
            });
        }
        let availability = self
            .route_health
            .lock()
            .await
            .reserve_route_health_probe(route, key);
        Ok(match availability {
            RouteAvailability::Ready(lease) => RouteAvailability::Ready(
                RouteHealthPermit::new_local(self.clone(), self.route_health.clone(), lease),
            ),
            RouteAvailability::Cooling {
                class,
                retry_after,
                upstream_status,
            } => RouteAvailability::Cooling {
                class,
                retry_after,
                upstream_status,
            },
            RouteAvailability::HalfOpenBusy {
                class,
                retry_after,
                upstream_status,
            } => RouteAvailability::HalfOpenBusy {
                class,
                retry_after,
                upstream_status,
            },
        })
    }

    pub async fn reserve_route_health(
        &self,
        route: &RouteHealthKey,
        key: &KeyHealthKey,
    ) -> Result<RouteAvailability<RouteHealthPermit>, RuntimeCoordinationError> {
        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            let availability = coordinator
                .reserve_route_health(route, key, &Uuid::new_v4().to_string())
                .await?;
            return Ok(match availability {
                RouteAvailability::Ready(lease) => RouteAvailability::Ready(
                    RouteHealthPermit::new_redis(self.clone(), coordinator.clone(), lease),
                ),
                RouteAvailability::Cooling {
                    class,
                    retry_after,
                    upstream_status,
                } => RouteAvailability::Cooling {
                    class,
                    retry_after,
                    upstream_status,
                },
                RouteAvailability::HalfOpenBusy {
                    class,
                    retry_after,
                    upstream_status,
                } => RouteAvailability::HalfOpenBusy {
                    class,
                    retry_after,
                    upstream_status,
                },
            });
        }
        let availability = self.route_health.lock().await.reserve(route, key);
        Ok(match availability {
            RouteAvailability::Ready(lease) => RouteAvailability::Ready(
                RouteHealthPermit::new_local(self.clone(), self.route_health.clone(), lease),
            ),
            RouteAvailability::Cooling {
                class,
                retry_after,
                upstream_status,
            } => RouteAvailability::Cooling {
                class,
                retry_after,
                upstream_status,
            },
            RouteAvailability::HalfOpenBusy {
                class,
                retry_after,
                upstream_status,
            } => RouteAvailability::HalfOpenBusy {
                class,
                retry_after,
                upstream_status,
            },
        })
    }

    pub async fn observe_route_failure(
        &self,
        route: &RouteHealthKey,
        class: RouteFailureClass,
        retry_after: Option<Duration>,
        shared_host_failure_domain: bool,
    ) -> Result<(), RuntimeCoordinationError> {
        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            return coordinator
                .observe_route_failure(route, class, retry_after, shared_host_failure_domain)
                .await;
        }
        self.route_health.lock().await.observe_route_failure(
            route,
            class,
            retry_after,
            shared_host_failure_domain,
        );
        Ok(())
    }

    pub async fn clear_route_health(
        &self,
        route: &RouteHealthKey,
    ) -> Result<(), RuntimeCoordinationError> {
        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            return coordinator.clear_route_health(route).await;
        }
        self.route_health.lock().await.clear_route_health(route);
        Ok(())
    }

    pub async fn observe_key_failure(
        &self,
        key: &KeyHealthKey,
        class: RouteFailureClass,
        retry_after: Option<Duration>,
    ) -> Result<(), RuntimeCoordinationError> {
        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            return coordinator
                .observe_key_failure(key, class, retry_after)
                .await;
        }
        self.route_health
            .lock()
            .await
            .observe_key_failure(key, class, retry_after);
        Ok(())
    }

    pub async fn observe_route_set_failure(
        &self,
        aggregate: &RouteSetAggregateKey,
        class: RouteFailureClass,
        retry_after: Option<Duration>,
    ) -> Result<(), RuntimeCoordinationError> {
        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            return coordinator
                .observe_route_set_failure(aggregate, class, retry_after)
                .await;
        }
        self.route_health
            .lock()
            .await
            .observe_route_set_failure(aggregate, class, retry_after);
        Ok(())
    }

    pub async fn earliest_temporary_route_recovery(
        &self,
        routes: &[RouteHealthKey],
    ) -> Result<Option<RouteRecovery>, RuntimeCoordinationError> {
        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            return coordinator.earliest_temporary_route_recovery(routes).await;
        }
        Ok(self
            .route_health
            .lock()
            .await
            .earliest_temporary_recovery(routes))
    }

    pub async fn route_health_snapshot(
        &self,
        route: &RouteHealthKey,
    ) -> Result<Option<HealthStateSnapshot>, RuntimeCoordinationError> {
        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            return coordinator.route_health_snapshot(route).await;
        }
        Ok(self.route_health.lock().await.route_health_snapshot(route))
    }

    pub async fn reset_upstream_route_health(
        &self,
        upstream_id: &str,
    ) -> Result<Option<usize>, RuntimeCoordinationError> {
        let upstream = self
            .snapshot()
            .await
            .upstreams
            .iter()
            .find(|upstream| upstream.id == upstream_id)
            .cloned();
        let Some(upstream) = upstream else {
            return Ok(None);
        };
        let routes = match &self.runtime_coordination {
            RuntimeCoordinationBackend::Redis(coordinator) => {
                coordinator
                    .configured_route_health_routes(&upstream)
                    .await?
            }
            RuntimeCoordinationBackend::Local => self
                .route_health
                .lock()
                .await
                .configured_route_health_routes(&upstream),
        };
        for route in &routes {
            self.clear_route_health(route).await?;
        }
        Ok(Some(routes.len()))
    }

    /// C5.2: force-release the local upstream concurrency gate for `upstream_id`:
    /// clear the leases it holds as if they had been released (optionally
    /// restricted to one `key_fingerprint`).  Waiting C3 queue requests poll
    /// for a free slot on a short interval, so clearing the leases unbars them
    /// within one poll tick — there is no explicit wake primitive to touch.
    /// Returns `Ok(None)` when the upstream does not exist, otherwise the
    /// number of leases cleared.  Redis backend clears all of the upstream's
    /// lease keys (the per-key filter is a local-backend facility).
    pub async fn reset_upstream_concurrency(
        &self,
        upstream_id: &str,
        key_fingerprint: Option<&str>,
    ) -> Result<Option<usize>, RuntimeCoordinationError> {
        let exists = self
            .snapshot()
            .await
            .upstreams
            .iter()
            .any(|upstream| upstream.id == upstream_id);
        if !exists {
            return Ok(None);
        }
        let cleared = match &self.runtime_coordination {
            RuntimeCoordinationBackend::Redis(coordinator) => {
                coordinator.reset_upstream_concurrency(upstream_id).await?
            }
            RuntimeCoordinationBackend::Local => self
                .upstream_lease_table
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove_leases_for_upstream(upstream_id, key_fingerprint),
        };
        Ok(Some(cleared))
    }

    pub async fn key_health_snapshot(
        &self,
        key: &KeyHealthKey,
    ) -> Result<Option<HealthStateSnapshot>, RuntimeCoordinationError> {
        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            return coordinator.key_health_snapshot(key).await;
        }
        Ok(self.route_health.lock().await.key_health_snapshot(key))
    }

    pub async fn route_set_health_snapshot(
        &self,
        aggregate: &RouteSetAggregateKey,
    ) -> Result<Option<HealthStateSnapshot>, RuntimeCoordinationError> {
        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            return coordinator.route_set_health_snapshot(aggregate).await;
        }
        Ok(self
            .route_health
            .lock()
            .await
            .route_set_health_snapshot(aggregate))
    }

    pub async fn route_health_snapshots(
        &self,
        upstreams: &[UpstreamConfig],
    ) -> Result<HashMap<String, RouteHealthSnapshotDto>, RuntimeCoordinationError> {
        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            return coordinator.route_health_snapshots(upstreams).await;
        }
        Ok(self.route_health.lock().await.upstream_snapshots(upstreams))
    }

    /// E5.4: cold per-route health detail (cooldown_until + last_failure_class
    /// + streak) for the admin surface.  The Redis backend keeps route health
    /// inside Lua and exposes only aggregate counts; per-route detail is a
    /// local-backend-only view for now.
    pub async fn route_health_detail_snapshots(&self) -> Vec<RouteHealthDetailDto> {
        if let RuntimeCoordinationBackend::Redis(_) = &self.runtime_coordination {
            return Vec::new();
        }
        self.route_health
            .lock()
            .await
            .route_health_detail_snapshots()
    }

    pub fn runtime_capability_hints_snapshot(&self) -> RuntimeCapabilityHintSnapshot {
        self.runtime_capability_hints
            .lock()
            .expect("runtime capability hint lock poisoned")
            .snapshot()
    }

    pub fn insert_runtime_capability_hint(
        &self,
        key: CapabilityHintKey,
        configuration_fingerprint: String,
    ) -> bool {
        self.runtime_capability_hints
            .lock()
            .expect("runtime capability hint lock poisoned")
            .insert(key, configuration_fingerprint)
    }

    pub fn clear_runtime_capability_hints_for_success(
        &self,
        profile: &DialectProfileKey,
        configuration_fingerprint: &str,
        capabilities: &BTreeSet<Capability>,
        requested_value: Option<&str>,
        protocol_succeeded: bool,
    ) {
        let mut hints = self
            .runtime_capability_hints
            .lock()
            .expect("runtime capability hint lock poisoned");
        hints.clear_features_for_success(
            profile,
            configuration_fingerprint,
            capabilities,
            requested_value,
        );
        if protocol_succeeded {
            hints.remove(&CapabilityHintKey::protocol(profile.clone()));
        }
    }

    pub fn clear_runtime_capability_hints_after_probe(
        &self,
        profile: &DialectProfileKey,
        configuration_fingerprint: &str,
        capabilities: &BTreeSet<Capability>,
    ) {
        self.runtime_capability_hints
            .lock()
            .expect("runtime capability hint lock poisoned")
            .clear_after_conclusive_probe(profile, configuration_fingerprint, capabilities);
    }

    pub fn reconcile_runtime_capability_hints(&self, upstreams: &[UpstreamConfig]) {
        let snapshot = self.capability_snapshot();
        let upstreams = upstreams.to_vec();
        self.runtime_capability_hints
            .lock()
            .expect("runtime capability hint lock poisoned")
            .retain_current(|key, fingerprint| {
                let Some(upstream) = upstreams
                    .iter()
                    .find(|candidate| candidate.id == key.profile.upstream_id && candidate.active)
                else {
                    return false;
                };
                let Some(protocol) = upstream_protocol_from_wire(key.profile.protocol) else {
                    return false;
                };
                let Some(api_key) = upstream.available_keys().into_iter().find(|api_key| {
                    upstream_key_fingerprint(&upstream.id, api_key) == key.profile.key_fingerprint
                }) else {
                    return false;
                };
                if !upstream
                    .keys_for_model(&key.profile.runtime_model_slug)
                    .iter()
                    .any(|candidate| candidate == &api_key)
                {
                    return false;
                }
                let exposed_models = upstream.effective_downstream_models();
                let exposed_models = if exposed_models.is_empty() {
                    vec![key.profile.runtime_model_slug.clone()]
                } else {
                    exposed_models
                };
                exposed_models.into_iter().any(|exposed_model| {
                    upstream.resolved_model_name(&exposed_model).as_deref()
                        == Some(key.profile.runtime_model_slug.as_str())
                        && Self::route_configuration_fingerprint_with_snapshot(
                            &snapshot,
                            upstream,
                            &key.profile.key_fingerprint,
                            &exposed_model,
                            &key.profile.runtime_model_slug,
                            protocol,
                        )
                        .is_ok_and(|current| current == fingerprint)
                })
            });
    }

    /// Reconcile health identities after a configuration change while preserving active
    /// half-open ownership long enough for its owner to finish cleanly.
    pub async fn reconcile_route_health(
        &self,
        upstreams: &[UpstreamConfig],
    ) -> Result<(), RuntimeCoordinationError> {
        let upstreams = upstreams.to_vec();
        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            coordinator.reconcile_route_health(&upstreams).await?;
        } else {
            self.route_health.lock().await.retain_routes(
                |route| route_health::route_health_route_is_current(&upstreams, route),
                |key| route_health::route_health_key_is_current(&upstreams, key),
                |aggregate| route_health::route_health_aggregate_is_current(&upstreams, aggregate),
            );
        }
        self.reconcile_runtime_capability_hints(&upstreams);
        self.reconcile_model_key_sync_runtime(&upstreams);
        Ok(())
    }

    pub fn create_admin_session(&self) -> String {
        let token = Uuid::new_v4().to_string();
        let expires_at = unix_seconds() + ADMIN_SESSION_TTL_SECONDS;
        let mut sessions = self
            .admin_sessions
            .lock()
            .expect("admin session lock poisoned");
        prune_expired_admin_sessions(&mut sessions);
        sessions.insert(token.clone(), expires_at);
        token
    }

    pub fn validate_admin_session(&self, token: &str) -> bool {
        let now = unix_seconds();
        let mut sessions = self
            .admin_sessions
            .lock()
            .expect("admin session lock poisoned");
        match sessions.get(token).copied() {
            Some(expires_at) if expires_at > now => true,
            Some(_) => {
                sessions.remove(token);
                false
            }
            None => false,
        }
    }

    pub fn revoke_admin_session(&self, token: &str) {
        let mut sessions = self
            .admin_sessions
            .lock()
            .expect("admin session lock poisoned");
        sessions.remove(token);
    }

    pub fn set_capability_probe_sender(&self, sender: mpsc::Sender<ProbeJobBatch>) {
        *self
            .capability_probe_sender
            .lock()
            .expect("probe sender lock poisoned") = Some(sender);
    }

    pub fn queue_capability_probe(&self, job: ProbeJob) -> bool {
        let runtime_settings = self.runtime_settings();
        if job.reason.is_automatic() && !runtime_settings.automatic_capability_probes_enabled {
            return false;
        }
        if !self
            .capability_snapshot()
            .configuration
            .source()
            .probe
            .enabled
        {
            return false;
        }
        matches!(
            self.try_queue_capability_probe(job),
            Ok(CapabilityProbeSubmission::Queued)
        )
    }

    fn try_queue_capability_probe(
        &self,
        job: ProbeJob,
    ) -> Result<CapabilityProbeSubmission, ManualProbeBatchError> {
        let sender = self
            .capability_probe_sender
            .lock()
            .expect("probe sender lock poisoned")
            .as_ref()
            .cloned()
            .ok_or(ManualProbeBatchError::QueueUnavailable)?;
        if sender.is_closed() {
            return Err(ManualProbeBatchError::QueueUnavailable);
        }
        let key = job.key.clone();
        let binding = job.configuration.clone();
        let mut submissions = self
            .capability_probe_submissions
            .lock()
            .expect("probe submission lock poisoned");
        if submissions
            .get(&key)
            .is_some_and(|queued| queued == &binding)
        {
            return Ok(CapabilityProbeSubmission::AlreadyPending);
        }
        let previous_binding = submissions.insert(key.clone(), binding.clone());
        match sender.try_send(ProbeJobBatch::single(job)) {
            Ok(()) => {
                drop(submissions);
                tracing::info!(
                    upstream_id = %key.upstream_id,
                    route_id = %anonymous_route_id(
                        &key.upstream_id,
                        &key.key_fingerprint,
                        &key.runtime_model_slug,
                        key.protocol,
                    ),
                    runtime_model = %key.runtime_model_slug,
                    protocol = ?key.protocol,
                    "capability probe queued"
                );
                Ok(CapabilityProbeSubmission::Queued)
            }
            Err(_) => {
                match previous_binding {
                    Some(previous_binding) => {
                        submissions.insert(key, previous_binding);
                    }
                    None => {
                        submissions.remove(&key);
                    }
                }
                Err(ManualProbeBatchError::QueueUnavailable)
            }
        }
    }

    pub async fn queue_manual_capability_probe(
        &self,
        job: ProbeJob,
    ) -> Result<(), ManualProbeBatchError> {
        let _persist_guard = self.config_persist_lock.lock().await;
        let _capability_guard = self.capability_update_lock.lock().await;
        let routing = self.routing_snapshot().await;
        let capability_snapshot = self.capability_snapshot();
        Self::validate_manual_capability_probe_configuration(
            capability_snapshot.configuration.source(),
        )?;
        if !Self::manual_capability_probe_job_is_current(&routing, &capability_snapshot, &job) {
            return Err(ManualProbeBatchError::NoEligibleRoutes);
        }
        match self.try_queue_capability_probe(job)? {
            CapabilityProbeSubmission::Queued | CapabilityProbeSubmission::AlreadyPending => Ok(()),
        }
    }

    pub fn validate_manual_capability_probe_policy(&self) -> Result<(), ManualProbeBatchError> {
        let capability_snapshot = self.capability_snapshot();
        Self::validate_manual_capability_probe_configuration(
            capability_snapshot.configuration.source(),
        )
    }

    fn validate_manual_capability_probe_configuration(
        configuration: &CapabilityConfiguration,
    ) -> Result<(), ManualProbeBatchError> {
        if configuration.revision == 0 {
            return Err(ManualProbeBatchError::CapabilityPolicyMissing);
        }
        if !configuration.probe.enabled {
            return Err(ManualProbeBatchError::CapabilityProbeDisabled);
        }
        Ok(())
    }

    pub async fn queue_manual_capability_probe_batch(
        &self,
        upstream_ids: &BTreeSet<String>,
        models: &BTreeSet<String>,
    ) -> Result<ManualProbeBatchReceipt, ManualProbeBatchError> {
        self.queue_manual_capability_probe_batch_with_mode(upstream_ids, models, ProbeMode::Full)
            .await
    }

    pub async fn queue_manual_capability_probe_batch_with_mode(
        &self,
        upstream_ids: &BTreeSet<String>,
        models: &BTreeSet<String>,
        mode: ProbeMode,
    ) -> Result<ManualProbeBatchReceipt, ManualProbeBatchError> {
        let _persist_guard = self.config_persist_lock.lock().await;
        let _capability_guard = self.capability_update_lock.lock().await;
        let routing = self.routing_snapshot().await;
        let capability_snapshot = self.capability_snapshot();
        let configuration = capability_snapshot.configuration.source();
        Self::validate_manual_capability_probe_configuration(configuration)?;

        let mut prepared_jobs = Vec::new();
        for upstream in routing.upstreams.iter().filter(|upstream| {
            upstream.active && (upstream_ids.is_empty() || upstream_ids.contains(&upstream.id))
        }) {
            for (key, exposed_model_slugs) in self.capability_probe_jobs_for_upstream(upstream) {
                let protocol = match key.protocol {
                    WireProtocol::ChatCompletions => UpstreamProtocol::ChatCompletions,
                    WireProtocol::Responses => UpstreamProtocol::Responses,
                    WireProtocol::Messages => continue,
                };
                let mut prepared_job = None;
                let mut prepared_model_slugs = BTreeSet::new();
                for exposed_model_slug in exposed_model_slugs
                    .into_iter()
                    .filter(|model| models.is_empty() || models.contains(model))
                {
                    let Ok(Some(job)) = Self::build_capability_probe_job_for_key_with_snapshot(
                        &capability_snapshot,
                        upstream,
                        &key.key_fingerprint,
                        &exposed_model_slug,
                        &key.runtime_model_slug,
                        protocol,
                        ProbeReason::Manual,
                        mode,
                    ) else {
                        continue;
                    };
                    prepared_job.get_or_insert(job);
                    prepared_model_slugs.insert(exposed_model_slug);
                }
                if let Some(mut job) = prepared_job {
                    job.exposed_model_slugs = prepared_model_slugs;
                    prepared_jobs.push(job);
                }
            }
        }
        prepared_jobs.sort_by(|left, right| left.key.cmp(&right.key));
        if prepared_jobs.is_empty() {
            return Err(ManualProbeBatchError::NoEligibleRoutes);
        }

        let sender = self
            .capability_probe_sender
            .lock()
            .expect("probe sender lock poisoned")
            .clone()
            .ok_or(ManualProbeBatchError::QueueUnavailable)?;

        let batch_id = Uuid::new_v4().to_string();
        let started_at = unix_seconds();
        let mut batch_models = BTreeSet::new();
        tracing::info!(
            batch_id = %batch_id,
            mode = ?mode,
            models = %serde_json::to_string(&models.iter().collect::<Vec<_>>()).unwrap_or_default(),
            "accepted manual capability probe batch request"
        );
        for job in &prepared_jobs {
            batch_models.extend(job.exposed_model_slugs.iter().cloned());
        }
        let batch_models = batch_models.into_iter().collect::<Vec<_>>();
        let mut queued_jobs = Vec::new();
        let mut queued_routes = 0;
        let mut reused_routes = 0;
        let mut candidates = Vec::with_capacity(prepared_jobs.len());
        {
            let mut submissions = self
                .capability_probe_submissions
                .lock()
                .expect("probe submission lock poisoned");
            for job in prepared_jobs {
                let state = if submissions
                    .get(&job.key)
                    .is_some_and(|binding| binding == &job.configuration)
                {
                    reused_routes += 1;
                    ManualProbeBatchCandidateState::Reused
                } else {
                    submissions.insert(job.key.clone(), job.configuration.clone());
                    queued_jobs.push(job.clone());
                    queued_routes += 1;
                    ManualProbeBatchCandidateState::Queued
                };
                let summary = ProbeCandidateSummary {
                    upstream_id: job.key.upstream_id.clone(),
                    route_id: anonymous_route_id(
                        &job.key.upstream_id,
                        &job.key.key_fingerprint,
                        &job.key.runtime_model_slug,
                        job.key.protocol,
                    ),
                    exposed_model_slug: job
                        .exposed_model_slugs
                        .iter()
                        .next()
                        .expect("prepared capability probe has an exposed model")
                        .clone(),
                    runtime_model_slug: job.key.runtime_model_slug.clone(),
                    protocol: job.key.protocol,
                    state,
                    diagnostic_code: None,
                };
                tracing::info!(
                    batch_id = %batch_id,
                    mode = ?mode,
                    upstream_id = %job.key.upstream_id,
                    route_id = %summary.route_id,
                    exposed_model_slug = %summary.exposed_model_slug,
                    runtime_model_slug = %summary.runtime_model_slug,
                    protocol = ?summary.protocol,
                    state = ?summary.state,
                    "capability probe batch candidate"
                );
                candidates.push(CapabilityProbeBatchCandidate {
                    summary,
                    key: job.key,
                    binding: job.configuration,
                });
            }
        }
        if !queued_jobs.is_empty() {
            if sender
                .try_send(ProbeJobBatch::new(queued_jobs.clone()))
                .is_err()
            {
                let mut submissions = self
                    .capability_probe_submissions
                    .lock()
                    .expect("probe submission lock poisoned");
                for job in queued_jobs {
                    if submissions
                        .get(&job.key)
                        .is_some_and(|binding| binding == &job.configuration)
                    {
                        submissions.remove(&job.key);
                    }
                }
                return Err(ManualProbeBatchError::QueueUnavailable);
            }
        }

        let status = CapabilityProbeBatchStatus {
            batch_id: batch_id.clone(),
            configuration_revision: configuration.revision,
            started_at,
            queued_routes,
            reused_routes,
            models: batch_models.clone(),
            estimated_remaining_seconds: None,
            candidates: candidates
                .iter()
                .map(|candidate| candidate.summary.clone())
                .collect(),
            terminal_at: (queued_routes + reused_routes == 0).then_some(started_at),
        };
        let receipt_candidates = status.candidates.clone();
        let mut batches = self
            .capability_probe_batches
            .lock()
            .expect("probe batch lock poisoned");
        prune_capability_probe_batches(&mut batches, started_at);
        batches.insert(
            batch_id.clone(),
            CapabilityProbeBatchRecord { status, candidates },
        );

        Ok(ManualProbeBatchReceipt {
            batch_id,
            configuration_revision: configuration.revision,
            started_at,
            queued_routes,
            reused_routes,
            models: batch_models,
            candidates: receipt_candidates,
        })
    }

    pub fn capability_probe_batch_status(
        &self,
        batch_id: &str,
    ) -> Option<CapabilityProbeBatchStatus> {
        let now = unix_seconds();
        let mut batches = self
            .capability_probe_batches
            .lock()
            .expect("probe batch lock poisoned");
        prune_capability_probe_batches(&mut batches, now);
        batches.get(batch_id).map(|record| {
            let mut status = record.status.clone();
            let settled = status
                .candidates
                .iter()
                .filter(|candidate| {
                    matches!(
                        candidate.state,
                        ManualProbeBatchCandidateState::Completed
                            | ManualProbeBatchCandidateState::Failed
                            | ManualProbeBatchCandidateState::CooldownSkipped
                            | ManualProbeBatchCandidateState::Superseded
                    )
                })
                .count();
            let total = status.candidates.len();
            if settled == 0 {
                status.estimated_remaining_seconds = None;
            } else if settled >= total {
                status.estimated_remaining_seconds = Some(0);
            } else {
                let elapsed = now.saturating_sub(status.started_at);
                let remaining = total - settled;
                status.estimated_remaining_seconds =
                    Some((elapsed * remaining as u64 / settled as u64).max(1));
            }
            status
        })
    }

    pub fn mark_capability_probe_running(
        &self,
        key: &DialectProfileKey,
        binding: &ProbeConfigurationBinding,
    ) {
        let mut batches = self
            .capability_probe_batches
            .lock()
            .expect("probe batch lock poisoned");
        for record in batches.values_mut() {
            for candidate in &mut record.candidates {
                if &candidate.key == key
                    && &candidate.binding == binding
                    && matches!(
                        candidate.summary.state,
                        ManualProbeBatchCandidateState::Queued
                            | ManualProbeBatchCandidateState::Reused
                    )
                {
                    candidate.summary.state = ManualProbeBatchCandidateState::Running;
                }
            }
            record.status.candidates = record
                .candidates
                .iter()
                .map(|candidate| candidate.summary.clone())
                .collect();
        }
    }

    pub fn finish_capability_probe_job(
        &self,
        key: &DialectProfileKey,
        binding: &ProbeConfigurationBinding,
        execution: &ProbeJobExecution,
    ) {
        let (state, diagnostic_code) = match execution {
            ProbeJobExecution::Completed => (ManualProbeBatchCandidateState::Completed, None),
            ProbeJobExecution::Failed(_) => (
                ManualProbeBatchCandidateState::Failed,
                Some("probe_execution_failed".to_string()),
            ),
            ProbeJobExecution::CooldownSkipped { .. } => (
                ManualProbeBatchCandidateState::CooldownSkipped,
                Some("skipped_route_cooling".to_string()),
            ),
            ProbeJobExecution::Superseded => (
                ManualProbeBatchCandidateState::Superseded,
                Some("probe_superseded".to_string()),
            ),
        };
        let now = unix_seconds();
        let mut batches = self
            .capability_probe_batches
            .lock()
            .expect("probe batch lock poisoned");
        for record in batches.values_mut() {
            let mut matched = false;
            for candidate in &mut record.candidates {
                if &candidate.key == key && &candidate.binding == binding {
                    candidate.summary.state = state.clone();
                    candidate.summary.diagnostic_code = diagnostic_code.clone();
                    matched = true;
                }
            }
            if matched {
                record.status.candidates = record
                    .candidates
                    .iter()
                    .map(|candidate| candidate.summary.clone())
                    .collect();
                if record.candidates.iter().all(|candidate| {
                    matches!(
                        candidate.summary.state,
                        ManualProbeBatchCandidateState::Completed
                            | ManualProbeBatchCandidateState::Failed
                            | ManualProbeBatchCandidateState::CooldownSkipped
                            | ManualProbeBatchCandidateState::Superseded
                    )
                }) {
                    record.status.terminal_at.get_or_insert(now);
                }
            }
        }
        self.finish_capability_probe_submission(key, binding);
    }

    pub fn finish_capability_probe_submission(
        &self,
        key: &DialectProfileKey,
        binding: &ProbeConfigurationBinding,
    ) {
        if self.remove_capability_probe_submission(key, binding) {
            tracing::info!(
                upstream_id = %key.upstream_id,
                route_id = %anonymous_route_id(
                    &key.upstream_id,
                    &key.key_fingerprint,
                    &key.runtime_model_slug,
                    key.protocol,
                ),
                runtime_model = %key.runtime_model_slug,
                protocol = ?key.protocol,
                "capability probe completed"
            );
        }
    }

    pub fn discard_capability_probe_submission(&self, job: &ProbeJob) {
        // A discarded job was superseded by a newer configuration or by the
        // probe feature being disabled; record it instead of silently
        // dropping it from the batch terminal state.
        self.finish_capability_probe_job(
            &job.key,
            &job.configuration,
            &ProbeJobExecution::Superseded,
        );
    }

    fn remove_capability_probe_submission(
        &self,
        key: &DialectProfileKey,
        binding: &ProbeConfigurationBinding,
    ) -> bool {
        let mut submissions = self
            .capability_probe_submissions
            .lock()
            .expect("probe submission lock poisoned");
        if submissions.get(key).is_some_and(|queued| queued == binding) {
            submissions.remove(key);
            true
        } else {
            false
        }
    }

    pub async fn build_capability_probe_job(
        &self,
        upstream_id: &str,
        key_fingerprint: &str,
        exposed_model_slug: &str,
        runtime_model_slug: &str,
        protocol: UpstreamProtocol,
        reason: ProbeReason,
    ) -> io::Result<Option<ProbeJob>> {
        let routing = self.routing_snapshot().await;
        let Some(upstream) = routing
            .upstreams
            .iter()
            .find(|upstream| upstream.id == upstream_id)
        else {
            return Ok(None);
        };
        let capability_snapshot = self.capability_snapshot();
        Self::build_capability_probe_job_for_key_with_snapshot(
            &capability_snapshot,
            upstream,
            key_fingerprint,
            exposed_model_slug,
            runtime_model_slug,
            protocol,
            reason,
            ProbeMode::Full,
        )
    }

    pub async fn build_manual_capability_probe_job(
        &self,
        upstream_id: &str,
        key_fingerprint: &str,
        exposed_model_slug: &str,
        runtime_model_slug: &str,
        protocol: UpstreamProtocol,
    ) -> Result<ProbeJob, ManualProbeBatchError> {
        let capability_snapshot = self.capability_snapshot();
        Self::validate_manual_capability_probe_configuration(
            capability_snapshot.configuration.source(),
        )?;
        let routing = self.routing_snapshot().await;
        let upstream = routing
            .upstreams
            .iter()
            .find(|upstream| upstream.id == upstream_id)
            .ok_or(ManualProbeBatchError::NoEligibleRoutes)?;
        Self::build_capability_probe_job_for_key_with_snapshot(
            &capability_snapshot,
            upstream,
            key_fingerprint,
            exposed_model_slug,
            runtime_model_slug,
            protocol,
            ProbeReason::Manual,
            ProbeMode::Full,
        )
        .ok()
        .flatten()
        .ok_or(ManualProbeBatchError::NoEligibleRoutes)
    }

    fn build_capability_probe_job_for_key_with_snapshot(
        capability_snapshot: &CapabilityRuntimeSnapshot,
        upstream: &UpstreamConfig,
        key_fingerprint: &str,
        exposed_model_slug: &str,
        runtime_model_slug: &str,
        protocol: UpstreamProtocol,
        reason: ProbeReason,
        mode: ProbeMode,
    ) -> io::Result<Option<ProbeJob>> {
        if !capability_snapshot.configuration.source().probe.enabled
            || !upstream.active
            || !upstream.supports_protocol(protocol)
            || upstream.resolved_model_name(exposed_model_slug).as_deref()
                != Some(runtime_model_slug)
            || !upstream
                .keys_for_model(runtime_model_slug)
                .iter()
                .any(|api_key| upstream_key_fingerprint(&upstream.id, api_key) == key_fingerprint)
        {
            return Ok(None);
        }
        let configuration_fingerprint = Self::route_configuration_fingerprint_with_snapshot(
            capability_snapshot,
            upstream,
            key_fingerprint,
            exposed_model_slug,
            runtime_model_slug,
            protocol,
        )?;
        Ok(Some(ProbeJob {
            key: DialectProfileKey::for_key(
                upstream.id.clone(),
                key_fingerprint,
                runtime_model_slug,
                protocol.into(),
            ),
            exposed_model_slugs: BTreeSet::from([exposed_model_slug.to_owned()]),
            reason,
            mode,
            configuration: ProbeConfigurationBinding {
                configuration_fingerprint,
                configuration_digest: capability_snapshot.configuration.digest().to_owned(),
                configuration_schema_version: capability_snapshot
                    .configuration
                    .source()
                    .schema_version,
                configuration_revision: capability_snapshot.configuration.source().revision,
                probe_schema_version: DIALECT_PROBE_SCHEMA_VERSION,
            },
            plan_configuration: capability_snapshot.configuration.clone(),
        }))
    }

    pub fn capability_probe_job_is_current(
        capability_snapshot: &CapabilityRuntimeSnapshot,
        upstream: &UpstreamConfig,
        job: &ProbeJob,
    ) -> bool {
        let Some(exposed_model_slug) = job.exposed_model_slugs.iter().next() else {
            return false;
        };
        let protocol = match job.key.protocol {
            WireProtocol::ChatCompletions => UpstreamProtocol::ChatCompletions,
            WireProtocol::Responses => UpstreamProtocol::Responses,
            WireProtocol::Messages => return false,
        };
        if !upstream.active
            || !upstream.supports_protocol(protocol)
            || job.exposed_model_slugs.iter().any(|model| {
                upstream.resolved_model_name(model).as_deref()
                    != Some(job.key.runtime_model_slug.as_str())
            })
            || !upstream
                .keys_for_model(&job.key.runtime_model_slug)
                .iter()
                .any(|api_key| {
                    upstream_key_fingerprint(&upstream.id, api_key) == job.key.key_fingerprint
                })
        {
            return false;
        }
        Self::capability_probe_configuration_binding_is_current(
            capability_snapshot,
            &job.configuration,
        ) && Self::route_configuration_fingerprint_with_snapshot(
            capability_snapshot,
            upstream,
            &job.key.key_fingerprint,
            exposed_model_slug,
            &job.key.runtime_model_slug,
            protocol,
        )
        .is_ok_and(|fingerprint| fingerprint == job.configuration.configuration_fingerprint)
    }

    fn manual_capability_probe_job_is_current(
        routing: &PersistedState,
        capability_snapshot: &CapabilityRuntimeSnapshot,
        job: &ProbeJob,
    ) -> bool {
        job.plan_configuration.digest() == job.configuration.configuration_digest
            && routing
                .upstreams
                .iter()
                .find(|upstream| upstream.id == job.key.upstream_id)
                .is_some_and(|upstream| {
                    Self::capability_probe_job_is_current(capability_snapshot, upstream, job)
                })
    }

    fn capability_probe_configuration_binding_is_current(
        capability_snapshot: &CapabilityRuntimeSnapshot,
        binding: &ProbeConfigurationBinding,
    ) -> bool {
        binding.configuration_schema_version
            == capability_snapshot.configuration.source().schema_version
            && binding.configuration_revision == capability_snapshot.configuration.source().revision
            && binding.configuration_digest == capability_snapshot.configuration.digest()
            && binding.probe_schema_version == DIALECT_PROBE_SCHEMA_VERSION
    }

    pub fn start_active_gateway_request(&self, start: ActiveGatewayRequestStart) {
        let now = unix_seconds();
        let mut active = self
            .active_requests
            .lock()
            .expect("active request lock poisoned");
        active.insert(
            start.request_id.clone(),
            ActiveGatewayRequest {
                request_id: start.request_id,
                downstream_id: start.downstream_id,
                downstream_name: start.downstream_name,
                endpoint: start.endpoint,
                model: start.model,
                protocol: start.protocol,
                user_agent: start.user_agent.map(truncate_active_request_user_agent),
                upstream_id: None,
                upstream_name: None,
                started_at: now,
                last_event_at: now,
                status: "routing".to_string(),
                error_category: None,
                // E5.2: entering the routing loop = still selecting a route.
                phase: "selecting".to_string(),
                queue_position: None,
            },
        );
    }

    pub fn mark_active_gateway_request_upstream(
        &self,
        request_id: &str,
        upstream_id: &str,
        upstream_name: &str,
    ) {
        let now = unix_seconds();
        let mut active = self
            .active_requests
            .lock()
            .expect("active request lock poisoned");
        if let Some(request) = active.get_mut(request_id) {
            request.upstream_id = Some(upstream_id.to_string());
            request.upstream_name = Some(upstream_name.to_string());
            request.last_event_at = now;
            request.status = "upstream".to_string();
            // E5.2: a real upstream was chosen and the request is sent out.
            request.phase = "dispatched".to_string();
            request.queue_position = None;
        }
    }

    /// E5.2: the request is parked in the C3 local-slot queue (local gate
    /// full, `upstream_account_queue_enabled`).  `queue_position` is the
    /// position in line as observed when the request joins (1-based best
    /// effort).  Distinguishes "queued behind a real slot" from "still
    /// choosing a route" — both previously looked like `upstream_id.is_none()`.
    pub fn mark_active_gateway_request_queued(&self, request_id: &str, queue_position: usize) {
        let now = unix_seconds();
        let mut active = self
            .active_requests
            .lock()
            .expect("active request lock poisoned");
        if let Some(request) = active.get_mut(request_id) {
            request.last_event_at = now;
            request.phase = "queued_local".to_string();
            request.queue_position = Some(queue_position.max(1));
        }
    }

    /// E5.2: first semantic output has not arrived past the E6 warn threshold
    /// — mark the request as stuck waiting for the first byte/token.  The
    /// timeout itself is unchanged (E6 only adds visibility).
    pub fn mark_active_gateway_request_awaiting_first_output(&self, request_id: &str) {
        let now = unix_seconds();
        let mut active = self
            .active_requests
            .lock()
            .expect("active request lock poisoned");
        if let Some(request) = active.get_mut(request_id) {
            request.last_event_at = now;
            request.phase = "awaiting_first_output".to_string();
        }
    }

    pub fn touch_active_gateway_request(&self, request_id: &str) {
        let now = unix_seconds();
        let mut active = self
            .active_requests
            .lock()
            .expect("active request lock poisoned");
        if let Some(request) = active.get_mut(request_id) {
            request.last_event_at = now;
            request.status = "streaming".to_string();
            request.phase = "streaming".to_string();
            request.queue_position = None;
        }
    }

    pub fn finish_active_gateway_request(&self, request_id: &str) {
        let mut active = self
            .active_requests
            .lock()
            .expect("active request lock poisoned");
        active.remove(request_id);
    }

    pub fn fail_active_gateway_request(&self, request_id: &str, error_category: impl Into<String>) {
        let mut active = self
            .active_requests
            .lock()
            .expect("active request lock poisoned");
        if let Some(request) = active.get_mut(request_id) {
            request.status = "error".to_string();
            request.error_category = Some(error_category.into());
            request.last_event_at = unix_seconds();
            // E5.1: a terminal retryable-capacity error handed to the client
            // is the retry-loop signal.  The categories are exactly the three
            // incident fingerprints: routes exhausted, local gate saturated,
            // and the upstream 429 family.  Keyed by (downstream_id, model).
            match request.error_category.as_deref() {
                Some(
                    "upstream_routes_exhausted"
                    | "gateway_concurrency_saturated"
                    | "upstream_rate_limited",
                ) => {
                    self.record_retry_terminal(&request.downstream_id, &request.model);
                }
                _ => {}
            }
        }
    }

    /// E5.1: record one retryable-capacity terminal error for
    /// `(downstream_id, model)` inside the sliding window, pruning entries
    /// older than [`RETRY_AMPLIFICATION_WINDOW_SECONDS`].
    fn record_retry_terminal(&self, downstream_id: &str, model: &str) {
        let now = unix_seconds();
        let window = RETRY_AMPLIFICATION_WINDOW_SECONDS;
        let mut counts = self
            .retry_terminal
            .lock()
            .expect("retry terminal lock poisoned");
        let bucket = counts
            .entry((downstream_id.to_string(), model.to_string()))
            .or_default();
        let cutoff = now.saturating_sub(window);
        while bucket.front().is_some_and(|t| *t <= cutoff) {
            bucket.pop_front();
        }
        bucket.push_back(now);
    }

    /// E5.1: snapshot of the retry-amplification metric over
    /// `window_seconds`, pruned and sorted by count descending.
    pub fn retry_terminal_snapshot(&self, window_seconds: u64) -> Vec<RetryTerminalPoint> {
        let now = unix_seconds();
        let window = window_seconds.max(1);
        let cutoff = now.saturating_sub(window);
        let mut counts = self
            .retry_terminal
            .lock()
            .expect("retry terminal lock poisoned");
        let mut points = Vec::new();
        for ((downstream_id, model), bucket) in counts.iter_mut() {
            while bucket.front().is_some_and(|t| *t <= cutoff) {
                bucket.pop_front();
            }
            if !bucket.is_empty() {
                points.push(RetryTerminalPoint {
                    downstream_id: downstream_id.clone(),
                    model: model.clone(),
                    count: bucket.len() as u64,
                });
            }
        }
        points.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then_with(|| a.downstream_id.cmp(&b.downstream_id))
                .then_with(|| a.model.cmp(&b.model))
        });
        points
    }

    pub fn active_gateway_requests(
        &self,
        downstream_filter: Option<&str>,
    ) -> Vec<ActiveGatewayRequestSnapshot> {
        let now = unix_seconds();
        let active = self
            .active_requests
            .lock()
            .expect("active request lock poisoned");
        let mut requests = active
            .values()
            .filter(|request| {
                downstream_filter
                    .map(|id| request.downstream_id == id)
                    .unwrap_or(true)
            })
            .map(|request| ActiveGatewayRequestSnapshot {
                request_id: request.request_id.clone(),
                downstream_id: request.downstream_id.clone(),
                downstream_name: request.downstream_name.clone(),
                endpoint: request.endpoint.clone(),
                model: request.model.clone(),
                protocol: request.protocol.clone(),
                user_agent: request.user_agent.clone(),
                upstream_id: request.upstream_id.clone(),
                upstream_name: request.upstream_name.clone(),
                started_at: request.started_at,
                last_event_at: request.last_event_at,
                elapsed_seconds: now.saturating_sub(request.started_at),
                idle_seconds: now.saturating_sub(request.last_event_at),
                status: request.status.clone(),
                error_category: request.error_category.clone(),
                phase: request.phase.clone(),
                queue_position: request.queue_position,
            })
            .collect::<Vec<_>>();
        requests.sort_by_key(|request| std::cmp::Reverse(request.started_at));
        requests
    }

    pub async fn snapshot(&self) -> PersistedState {
        let mut state = self.inner.lock().await.clone();
        let pending_usage_logs = self.pending_usage_logs.lock().await.clone();
        let archived_usage_logs = self.archived_usage_logs.lock().await.clone();
        if archived_usage_logs.is_empty() && pending_usage_logs.is_empty() {
            return state;
        }

        let mut usage_logs = pending_usage_logs
            .into_iter()
            .chain(archived_usage_logs)
            .chain(state.usage_logs)
            .collect::<Vec<_>>();
        usage_logs.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then(left.request_id.cmp(&right.request_id))
                .then(left.id.cmp(&right.id))
        });
        let mut seen = HashSet::new();
        let mut deduped = Vec::with_capacity(usage_logs.len());
        for log in usage_logs {
            if seen.insert(log.id.clone()) {
                deduped.push(log);
            }
        }
        state.usage_logs = deduped;
        state
    }

    pub async fn dashboard_usage_logs(
        &self,
        start_time: u64,
        end_time: u64,
    ) -> io::Result<Vec<UsageLog>> {
        let pending = self.pending_usage_logs.lock().await.clone();
        let durable = self
            .config_store
            .query_usage_logs_window(start_time, end_time)
            .await?
            .unwrap_or_default();
        let snapshot = self.snapshot().await;

        let mut seen = HashSet::new();
        let mut logs = pending
            .into_iter()
            .chain(durable)
            .chain(snapshot.usage_logs)
            .filter(|log| log.created_at >= start_time && log.created_at < end_time)
            .filter(|log| seen.insert(log.id.clone()))
            .collect::<Vec<_>>();
        logs.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then(left.request_id.cmp(&right.request_id))
                .then(left.id.cmp(&right.id))
        });
        Ok(logs)
    }

    /// Look up a single downstream configuration without cloning usage logs.
    ///
    /// Request-teardown billing only needs the downstream's own billing fields;
    /// `snapshot()` would deep-copy every usage log in the window for it.
    pub async fn downstream_config(&self, downstream_id: &str) -> Option<DownstreamConfig> {
        let state = self.inner.lock().await;
        state
            .downstreams
            .iter()
            .find(|downstream| downstream.id == downstream_id)
            .cloned()
    }

    pub async fn routing_snapshot(&self) -> PersistedState {
        let state = self.inner.lock().await;
        PersistedState {
            upstreams: state.upstreams.clone(),
            downstreams: state.downstreams.clone(),
            usage_logs: Vec::new(),
            announcement: None,
            global_context_profiles: state.global_context_profiles.clone(),
            runtime_settings: state.runtime_settings.clone(),
            model_aliases: state.model_aliases.clone(),
        }
    }

    pub fn runtime_settings(&self) -> Arc<RuntimeSettings> {
        self.runtime_settings.load_full()
    }

    pub fn startup_runtime_settings(&self) -> &RuntimeSettings {
        &self.startup_runtime_settings
    }

    pub fn model_alias_registry(&self) -> Arc<model_identity::ModelAliasRegistry> {
        self.model_alias_registry.load_full()
    }

    fn runtime_settings_document(
        &self,
        state: &PersistedState,
    ) -> (RuntimeSettingsSource, RuntimeSettingsDocument) {
        if let Some(document) = state
            .runtime_settings
            .as_ref()
            .filter(|document| document.schema_version == RUNTIME_SETTINGS_SCHEMA_VERSION)
            .and_then(|document| {
                document
                    .settings
                    .clone()
                    .validate_and_normalize()
                    .ok()
                    .map(|settings| RuntimeSettingsDocument {
                        settings,
                        ..document.clone()
                    })
            })
        {
            return (RuntimeSettingsSource::Persisted, document);
        }

        (
            RuntimeSettingsSource::Startup,
            RuntimeSettingsDocument {
                schema_version: RUNTIME_SETTINGS_SCHEMA_VERSION,
                revision: 0,
                updated_at: 0,
                settings: self.runtime_settings.load_full().as_ref().clone(),
            },
        )
    }

    fn runtime_settings_response_for(
        &self,
        source: RuntimeSettingsSource,
        document: RuntimeSettingsDocument,
    ) -> RuntimeSettingsResponse {
        let restart_required_fields = differing_runtime_setting_fields(
            self.startup_runtime_settings.as_ref(),
            &document.settings,
            RESTART_RUNTIME_SETTING_FIELDS,
        );
        RuntimeSettingsResponse {
            schema_version: document.schema_version,
            revision: document.revision,
            source,
            settings: document.settings,
            restart_required: !restart_required_fields.is_empty(),
            restart_required_fields,
        }
    }

    /// Backend identifier of the active config store ("postgres" or "file"),
    /// surfaced in admin persistence-failure details for operator triage.
    pub fn config_store_backend_name(&self) -> &'static str {
        self.config_store.backend_name()
    }

    pub async fn runtime_settings_response(&self) -> RuntimeSettingsResponse {
        let state = self.inner.lock().await;
        let (source, document) = self.runtime_settings_document(&state);
        self.runtime_settings_response_for(source, document)
    }

    pub async fn update_runtime_settings(
        &self,
        expected_revision: u64,
        settings: RuntimeSettings,
    ) -> Result<RuntimeSettingsUpdate, RuntimeSettingsUpdateError> {
        let settings = settings.validate_and_normalize()?;
        let _persist_guard = self.config_persist_lock.lock().await;
        let mut state = self.inner.lock().await;
        let (_, current_document) = self.runtime_settings_document(&state);

        if current_document.revision != expected_revision {
            return Err(RuntimeSettingsUpdateError::RevisionConflict {
                expected_revision,
                current_revision: current_document.revision,
            });
        }

        let revision = current_document
            .revision
            .checked_add(1)
            .ok_or(RuntimeSettingsUpdateError::RevisionExhausted)?;
        let applied_immediately = differing_runtime_setting_fields(
            &current_document.settings,
            &settings,
            IMMEDIATE_RUNTIME_SETTING_FIELDS,
        );
        let document = RuntimeSettingsDocument {
            schema_version: RUNTIME_SETTINGS_SCHEMA_VERSION,
            revision,
            updated_at: unix_seconds(),
            settings: settings.clone(),
        };
        let mut candidate_state = state.clone();
        candidate_state.runtime_settings = Some(document.clone());

        self.config_store
            .persist_config(&candidate_state)
            .await
            .map_err(RuntimeSettingsUpdateError::Persist)?;

        state.runtime_settings = Some(document.clone());
        {
            let mut route_health = self.route_health.lock().await;
            route_health.update_runtime_tuning(
                settings.upstream_concurrency_probe_delays_ms.clone(),
                settings.upstream_transient_route_cooldown_base_seconds,
                settings.upstream_transient_route_cooldown_max_seconds,
                settings.upstream_transient_route_cooldown_max_step,
                settings.upstream_route_health_half_open_ttl_seconds,
                settings.upstream_route_half_open_exclusive_window_ms,
                settings.upstream_credentials_first_strike_seconds,
                settings.upstream_capacity_failure_cooldown_enabled,
            );
        }
        self.account_concurrency.update_runtime_tuning(
            settings.upstream_concurrency_probe_delays_ms.clone(),
            settings.upstream_concurrency_recovery_max_wait_ms,
        );
        self.runtime_coordination.update_runtime_tuning(&settings);
        self.runtime_settings.store(Arc::new(settings));
        let response =
            self.runtime_settings_response_for(RuntimeSettingsSource::Persisted, document);
        tracing::info!(
            revision,
            applied_immediately = ?applied_immediately,
            restart_required_fields = ?response.restart_required_fields,
            "updated runtime settings"
        );

        Ok(RuntimeSettingsUpdate {
            response,
            applied_immediately,
        })
    }

    pub async fn update_model_aliases(
        &self,
        rules: Vec<model_identity::ModelAliasRule>,
    ) -> Result<(), String> {
        // Validate rules before persisting
        let registry = model_identity::ModelAliasRegistry::from_rules(rules.clone())?;

        let _persist_guard = self.config_persist_lock.lock().await;
        let mut state = self.inner.lock().await;
        let mut candidate_state = state.clone();
        for upstream in candidate_state.upstreams.iter() {
            if let Err(validation_error) =
                upstream.validate_model_mappings_against_aliases(&registry)
            {
                if let Some((mapping, canonical)) =
                    upstream.model_mappings.iter().find_map(|mapping| {
                        registry
                            .resolve_alias(&mapping.downstream_model)
                            .map(|canonical| (mapping, canonical))
                    })
                {
                    return Err(format!(
                        "upstream '{}' ({}) model mapping '{}' -> '{}' conflicts with canonical alias rule '{}': {validation_error}",
                        upstream.name,
                        upstream.id,
                        mapping.upstream_model,
                        mapping.downstream_model,
                        canonical,
                    ));
                }
                return Err(format!(
                    "upstream '{}' ({}) has an invalid model mapping: {validation_error}",
                    upstream.name, upstream.id,
                ));
            }
        }
        candidate_state.model_aliases = rules;

        self.config_store
            .persist_config(&candidate_state)
            .await
            .map_err(|error| format!("failed to persist model aliases: {}", error))?;

        state.model_aliases = candidate_state.model_aliases.clone();
        self.model_alias_registry.store(Arc::new(registry));

        tracing::info!(
            rule_count = candidate_state.model_aliases.len(),
            "updated model alias rules"
        );

        Ok(())
    }

    pub fn capability_snapshot(&self) -> Arc<CapabilityRuntimeSnapshot> {
        self.capability_snapshot.load_full()
    }

    pub async fn load_from_path(path: impl AsRef<Path>, config: AppConfig) -> io::Result<Self> {
        let deployment_calendar = DeploymentCalendar::parse(&config.deployment_timezone)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        Self::load_from_path_with_calendar(path, config, deployment_calendar).await
    }

    pub async fn load_from_path_with_calendar(
        path: impl AsRef<Path>,
        config: AppConfig,
        deployment_calendar: DeploymentCalendar,
    ) -> io::Result<Self> {
        if let Ok(database_url) = env::var("DATABASE_URL") {
            if !database_url.trim().is_empty() {
                tracing::info!(backend = "postgres", "loading gateway state from postgres");
                return Self::load_from_database_url_with_calendar(
                    database_url,
                    config,
                    deployment_calendar,
                )
                .await;
            }
        }

        let store_path = path.as_ref().to_path_buf();
        tracing::info!(
            backend = "file",
            state_path = %store_path.display(),
            "loading gateway state from file"
        );
        let state = if fs::try_exists(&store_path).await? {
            let bytes = fs::read(&store_path).await?;
            serde_json::from_slice(&bytes).unwrap_or_default()
        } else {
            PersistedState::default()
        };
        let (config, _) = config_with_persisted_runtime_settings(&state, config);
        let runtime_coordination = RuntimeCoordinationBackend::from_config(&config).await?;

        let archived_usage_logs = load_archived_usage_logs(&store_path).await?;
        let upstream_count = state.upstreams.len();
        let downstream_count = state.downstreams.len();
        let usage_log_count = state.usage_logs.len();
        let archived_usage_log_count = archived_usage_logs.len();
        let app = Self::new_with_archived(
            state,
            archived_usage_logs,
            store_path,
            config,
            runtime_coordination,
            deployment_calendar,
        );
        app.repair_legacy_local_admission_route_health_at_startup()
            .await?;
        let capability_state = app.config_store.load_capability_state().await?;
        app.initialize_capability_snapshot_from_store(capability_state)
            .await?;
        app.enforce_usage_log_archive_limit().await?;
        tracing::info!(
            backend = "file",
            state_path = %app.store_path.display(),
            upstreams = upstream_count,
            downstreams = downstream_count,
            usage_logs = usage_log_count,
            archived_usage_logs = archived_usage_log_count,
            "loaded file-backed gateway state"
        );
        Ok(app)
    }

    pub async fn load_from_database_url(
        database_url: impl AsRef<str>,
        config: AppConfig,
    ) -> io::Result<Self> {
        let deployment_calendar = DeploymentCalendar::parse(&config.deployment_timezone)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        Self::load_from_database_url_with_calendar(database_url, config, deployment_calendar).await
    }

    async fn load_from_database_url_with_calendar(
        database_url: impl AsRef<str>,
        config: AppConfig,
        deployment_calendar: DeploymentCalendar,
    ) -> io::Result<Self> {
        let postgres =
            PostgresStateStore::connect(database_url.as_ref(), config.postgres_pool_max_size)
                .await
                .map_err(|error| {
                    io::Error::other(format!("failed to initialize postgres backend: {error}"))
                })?;
        let state = postgres.load_state().await?;
        let capability_state = postgres.load_capability_state().await?;
        let (config, _) = config_with_persisted_runtime_settings(&state, config);
        let runtime_coordination = RuntimeCoordinationBackend::from_config(&config).await?;
        tracing::info!(
            backend = "postgres",
            upstreams = state.upstreams.len(),
            downstreams = state.downstreams.len(),
            usage_logs = state.usage_logs.len(),
            "loaded postgres-backed gateway state"
        );
        let app = Self::new_with_postgres(
            state,
            config,
            postgres,
            runtime_coordination,
            deployment_calendar,
        )
        .await;
        app.repair_legacy_local_admission_route_health_at_startup()
            .await?;
        app.initialize_capability_snapshot_from_store(capability_state)
            .await?;
        Ok(app)
    }

    pub async fn persist(&self) -> io::Result<()> {
        let state = self.snapshot().await;
        self.persist_state(&state).await
    }

    pub async fn replace_capability_configuration(
        &self,
        configuration: CapabilityConfiguration,
    ) -> io::Result<()> {
        let _guard = self.capability_update_lock.lock().await;
        let compiled = configuration
            .compile()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        self.config_store
            .persist_capability_configuration(&configuration)
            .await?;
        let current = self.capability_snapshot();
        self.capability_snapshot
            .store(Arc::new(CapabilityRuntimeSnapshot {
                configuration: Arc::new(compiled),
                profiles: current.profiles.clone(),
            }));
        Ok(())
    }

    pub async fn upsert_dialect_profile(&self, profile: UpstreamDialectProfile) -> io::Result<()> {
        let _guard = self.capability_update_lock.lock().await;
        self.config_store.upsert_dialect_profile(&profile).await?;
        let current = self.capability_snapshot();
        let mut profiles = current.profiles.clone();
        profiles.insert(profile.key.clone(), profile);
        self.capability_snapshot
            .store(Arc::new(CapabilityRuntimeSnapshot {
                configuration: current.configuration.clone(),
                profiles,
            }));
        Ok(())
    }

    pub async fn upsert_dialect_profile_if_probe_current(
        &self,
        profile: UpstreamDialectProfile,
        binding: &ProbeConfigurationBinding,
    ) -> io::Result<bool> {
        let _guard = self.capability_update_lock.lock().await;
        let current = self.capability_snapshot();
        if !Self::capability_probe_configuration_binding_is_current(&current, binding)
            || profile.configuration_fingerprint != binding.configuration_fingerprint
            || profile.probe_schema_version != binding.probe_schema_version
        {
            return Ok(false);
        }
        self.config_store.upsert_dialect_profile(&profile).await?;
        let mut profiles = current.profiles.clone();
        profiles.insert(profile.key.clone(), profile);
        self.capability_snapshot
            .store(Arc::new(CapabilityRuntimeSnapshot {
                configuration: current.configuration.clone(),
                profiles,
            }));
        Ok(true)
    }

    pub async fn learn_stream_only_route(
        &self,
        key: &DialectProfileKey,
        exposed_model_slug: &str,
        configuration_fingerprint: &str,
    ) -> io::Result<bool> {
        let _persist_guard = self.config_persist_lock.lock().await;
        let _capability_guard = self.capability_update_lock.lock().await;
        let latest = self.config_store.load_capability_state().await?;
        let compiled = Arc::new(
            latest
                .configuration
                .compile()
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
        );
        let latest_snapshot = CapabilityRuntimeSnapshot {
            configuration: compiled.clone(),
            profiles: latest.profiles.clone(),
        };
        let routing = self.routing_snapshot().await;
        let Some(upstream) = routing
            .upstreams
            .iter()
            .find(|upstream| upstream.id == key.upstream_id)
        else {
            return Ok(false);
        };
        let protocol = match key.protocol {
            WireProtocol::ChatCompletions => UpstreamProtocol::ChatCompletions,
            WireProtocol::Responses => UpstreamProtocol::Responses,
            WireProtocol::Messages => return Ok(false),
        };
        if !upstream.supports_model(exposed_model_slug)
            || !upstream.supports_stored_model(&key.runtime_model_slug)
        {
            return Ok(false);
        }
        if !upstream.supports_protocol(protocol) {
            return Ok(false);
        }
        let effective_key_fingerprint = if key.key_fingerprint.is_empty() {
            first_model_key_fingerprint(upstream, &key.runtime_model_slug).unwrap_or_default()
        } else {
            key.key_fingerprint.clone()
        };
        if !upstream
            .keys_for_model(&key.runtime_model_slug)
            .iter()
            .any(|api_key| {
                upstream_key_fingerprint(&upstream.id, api_key) == effective_key_fingerprint
            })
        {
            return Ok(false);
        }
        let latest_fingerprint = Self::route_configuration_fingerprint_with_snapshot(
            &latest_snapshot,
            upstream,
            &effective_key_fingerprint,
            exposed_model_slug,
            &key.runtime_model_slug,
            protocol,
        )?;
        if latest_fingerprint != configuration_fingerprint {
            return Ok(false);
        }
        let mut profile = if let Some(current_profile) = latest.profiles.get(key) {
            if current_profile.key != *key
                || current_profile.configuration_fingerprint != configuration_fingerprint
                || current_profile.probe_schema_version != DIALECT_PROBE_SCHEMA_VERSION
            {
                return Ok(false);
            }
            current_profile.clone()
        } else {
            let mut profile = UpstreamDialectProfile::unknown(key.clone());
            profile.configuration_fingerprint = configuration_fingerprint.to_string();
            profile
        };
        profile
            .capabilities
            .insert(Capability::NonStreamingResponse, EvidenceState::Rejected);
        profile
            .capabilities
            .insert(Capability::TextStream, EvidenceState::Supported);
        self.config_store.upsert_dialect_profile(&profile).await?;

        let mut profiles = latest.profiles;
        profiles.insert(key.clone(), profile);
        self.capability_snapshot
            .store(Arc::new(CapabilityRuntimeSnapshot {
                configuration: compiled,
                profiles,
            }));
        Ok(true)
    }

    /// Persist a dialect-field rejection learned from a successful same-route
    /// downgrade retry (A3): the capability backing the rejected field is
    /// marked Rejected on the route profile so later requests omit the field
    /// on the first attempt instead of probing the upstream again.
    pub async fn learn_dialect_field_rejected(
        &self,
        key: &DialectProfileKey,
        exposed_model_slug: &str,
        configuration_fingerprint: &str,
        capability: Capability,
    ) -> io::Result<bool> {
        let _persist_guard = self.config_persist_lock.lock().await;
        let _capability_guard = self.capability_update_lock.lock().await;
        let latest = self.config_store.load_capability_state().await?;
        let compiled = Arc::new(
            latest
                .configuration
                .compile()
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
        );
        let latest_snapshot = CapabilityRuntimeSnapshot {
            configuration: compiled.clone(),
            profiles: latest.profiles.clone(),
        };
        let routing = self.routing_snapshot().await;
        let Some(upstream) = routing
            .upstreams
            .iter()
            .find(|upstream| upstream.id == key.upstream_id)
        else {
            return Ok(false);
        };
        let protocol = match key.protocol {
            WireProtocol::ChatCompletions => UpstreamProtocol::ChatCompletions,
            WireProtocol::Responses => UpstreamProtocol::Responses,
            WireProtocol::Messages => return Ok(false),
        };
        if !upstream.supports_model(exposed_model_slug)
            || !upstream.supports_stored_model(&key.runtime_model_slug)
        {
            return Ok(false);
        }
        if !upstream.supports_protocol(protocol) {
            return Ok(false);
        }
        let effective_key_fingerprint = if key.key_fingerprint.is_empty() {
            first_model_key_fingerprint(upstream, &key.runtime_model_slug).unwrap_or_default()
        } else {
            key.key_fingerprint.clone()
        };
        if !upstream
            .keys_for_model(&key.runtime_model_slug)
            .iter()
            .any(|api_key| {
                upstream_key_fingerprint(&upstream.id, api_key) == effective_key_fingerprint
            })
        {
            return Ok(false);
        }
        let latest_fingerprint = Self::route_configuration_fingerprint_with_snapshot(
            &latest_snapshot,
            upstream,
            &effective_key_fingerprint,
            exposed_model_slug,
            &key.runtime_model_slug,
            protocol,
        )?;
        if latest_fingerprint != configuration_fingerprint {
            return Ok(false);
        }
        let mut profile = if let Some(current_profile) = latest.profiles.get(key) {
            if current_profile.key != *key
                || current_profile.configuration_fingerprint != configuration_fingerprint
                || current_profile.probe_schema_version != DIALECT_PROBE_SCHEMA_VERSION
            {
                return Ok(false);
            }
            current_profile.clone()
        } else {
            let mut profile = UpstreamDialectProfile::unknown(key.clone());
            profile.configuration_fingerprint = configuration_fingerprint.to_string();
            profile
        };
        profile
            .capabilities
            .insert(capability, EvidenceState::Rejected);
        self.config_store.upsert_dialect_profile(&profile).await?;

        let mut profiles = latest.profiles;
        profiles.insert(key.clone(), profile);
        self.capability_snapshot
            .store(Arc::new(CapabilityRuntimeSnapshot {
                configuration: compiled,
                profiles,
            }));
        Ok(true)
    }

    pub async fn delete_dialect_profiles_for_upstream(&self, upstream_id: &str) -> io::Result<()> {
        let _guard = self.capability_update_lock.lock().await;
        self.config_store
            .delete_dialect_profiles_for_upstream(upstream_id)
            .await?;
        let current = self.capability_snapshot();
        let mut profiles = current.profiles.clone();
        profiles.retain(|key, _| key.upstream_id != upstream_id);
        self.capability_snapshot
            .store(Arc::new(CapabilityRuntimeSnapshot {
                configuration: current.configuration.clone(),
                profiles,
            }));
        Ok(())
    }

    async fn delete_dialect_profile(&self, key: &DialectProfileKey) -> io::Result<()> {
        let _guard = self.capability_update_lock.lock().await;
        self.config_store.delete_dialect_profile(key).await?;
        let current = self.capability_snapshot();
        let mut profiles = current.profiles.clone();
        profiles.remove(key);
        self.capability_snapshot
            .store(Arc::new(CapabilityRuntimeSnapshot {
                configuration: current.configuration.clone(),
                profiles,
            }));
        Ok(())
    }

    pub async fn update_announcement(
        &self,
        announcement: Option<AnnouncementConfig>,
    ) -> io::Result<()> {
        self.mutate_persisted_state_io(move |state| {
            state.announcement = announcement;
            Ok(())
        })
        .await
    }

    pub async fn set_global_context_profiles(
        &self,
        global_context_profiles: std::collections::HashMap<String, GlobalContextProfile>,
    ) -> io::Result<()> {
        let global_context_profiles =
            normalize_global_context_profiles_for_storage(global_context_profiles);

        self.mutate_persisted_state_io(move |state| {
            state.global_context_profiles = Arc::new(global_context_profiles);
            Ok(())
        })
        .await
    }

    pub async fn announcement(&self) -> Option<AnnouncementConfig> {
        self.snapshot().await.announcement
    }

    pub async fn downstream_for_secret(&self, secret: &str) -> Option<DownstreamConfig> {
        // Split matching into a cheap in-lock pass and an expensive out-of-lock pass.
        //
        // The plaintext fast path is a constant-time byte compare, so it stays under
        // the lock. Downstreams that carry only an Argon2 hash require `verify_password`,
        // which is deliberately expensive (CPU-bound, tens of ms). Running that under
        // `inner.lock()` would serialize every incoming request behind a single auth and
        // starve the async executor, so we clone those candidates out, release the lock,
        // and verify them off the async path via `spawn_blocking`.
        let mut hashed_candidates: Vec<DownstreamConfig> = Vec::new();
        {
            let state = self.inner.lock().await;
            for downstream in state.downstreams.iter().filter(|d| d.active) {
                match downstream.plaintext_key.as_deref() {
                    Some(plaintext) => {
                        if plaintext.as_bytes().ct_eq(secret.as_bytes()).into() {
                            return Some(downstream.clone());
                        }
                    }
                    None => hashed_candidates.push(downstream.clone()),
                }
            }
        }

        if hashed_candidates.is_empty() {
            return None;
        }

        // Argon2 verification is CPU-bound; run it on a blocking thread so it neither
        // holds the state lock nor blocks the async worker.
        let secret = secret.to_owned();
        tokio::task::spawn_blocking(move || {
            hashed_candidates
                .into_iter()
                .find(|downstream| verify_downstream_key(&secret, &downstream.hash))
        })
        .await
        .unwrap_or(None)
    }

    pub async fn fetch_models_from_endpoint(
        &self,
        base_url: &str,
        api_key: &str,
    ) -> Result<Vec<String>, String> {
        let url = join_upstream_url(base_url, "/v1/models");
        let response = self
            .client_for_url(&url)
            .get(url)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", api_key.trim()),
            )
            .send()
            .await
            .map_err(|error| format!("请求上游模型失败: {error}"))?;

        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|error| format!("读取上游模型响应失败: {error}"))?;

        if !status.is_success() {
            let body = String::from_utf8_lossy(&bytes);
            return Err(format!(
                "上游返回状态 {}{}",
                status,
                if body.trim().is_empty() {
                    String::new()
                } else {
                    format!(": {body}")
                }
            ));
        }

        let payload: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("解析上游模型响应失败: {error}"))?;
        let mut seen = HashSet::new();
        let mut models = Vec::new();

        if let Some(items) = payload.get("data").and_then(Value::as_array) {
            for item in items {
                if let Some(model) = item.get("id").and_then(Value::as_str) {
                    let model = model.trim();
                    if !model.is_empty() && seen.insert(model.to_string()) {
                        models.push(model.to_string());
                    }
                }
            }
        }

        Ok(models)
    }

    pub async fn choose_upstream(
        &self,
        model: &str,
        protocol: UpstreamProtocol,
    ) -> Result<UpstreamConfig, RouteError> {
        let selected = select_upstream_with_model_matching(
            &RouteRequest::new(model, protocol, false),
            &self.upstream_candidates().await,
            self.runtime_settings().model_case_insensitive_matching,
        )?;

        let state = self.inner.lock().await;
        state
            .upstreams
            .iter()
            .find(|upstream| upstream.id == selected.id)
            .cloned()
            .ok_or(RouteError::NoHealthyUpstream(model.to_string()))
    }

    pub async fn upstream_candidates(&self) -> Vec<UpstreamCandidate> {
        let state = self.inner.lock().await;
        state
            .upstreams
            .iter()
            .filter(|upstream| upstream.active)
            .flat_map(|upstream| {
                upstream
                    .supported_protocols()
                    .into_iter()
                    .map(|protocol| {
                        UpstreamCandidate::new(upstream.id.clone(), upstream.name.clone(), protocol)
                            .with_models(upstream.route_models())
                            .with_priority(upstream.priority)
                            .with_premium_models(upstream.premium_route_models())
                            .with_failure_count(upstream.failure_count)
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    pub async fn try_reserve_upstream_request(
        &self,
        upstream: &UpstreamConfig,
        model: &str,
    ) -> Result<UpstreamRequestLease, UpstreamAdmissionError> {
        let key_fingerprint = upstream_key_fingerprint(&upstream.id, &upstream.api_key);
        self.try_reserve_upstream_account_request(upstream, &key_fingerprint, model)
            .await
    }

    pub async fn try_reserve_upstream_account_request(
        &self,
        upstream: &UpstreamConfig,
        key_fingerprint: &str,
        _model: &str,
    ) -> Result<UpstreamRequestLease, UpstreamAdmissionError> {
        // Upstream model-weighted request costs were removed; every request
        // consumes a flat 1.0 quota unit.
        let request_cost = 1.0_f64;
        let account = AccountConcurrencyKey::new(upstream.id.clone(), key_fingerprint);

        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            let lease_id = Uuid::new_v4().to_string();
            coordinator
                .reserve_upstream_request(
                    upstream,
                    &account,
                    request_cost,
                    &Uuid::new_v4().to_string(),
                    &lease_id,
                    false,
                )
                .await?;
            return Ok(UpstreamRequestLease {
                account,
                lease_id,
                release_state: Arc::new(AtomicU8::new(LEASE_RELEASE_ACTIVE)),
            });
        }

        let mut runtime_state = self.upstream_runtime_state.lock().await;
        let state = runtime_state
            .entry(upstream.id.clone())
            .or_insert_with(UpstreamRuntimeState::default);

        let now_instant = tokio::time::Instant::now();
        let now = unix_seconds();
        prune_quota_events(&mut state.minute_events, now, 60);
        prune_quota_events(
            &mut state.five_hour_events,
            now,
            upstream.request_quota_window_seconds(),
        );

        // C1.1: leases live in the separate `upstream_lease_table` behind a
        // plain `std::sync::Mutex`; the critical section is purely synchronous
        // (no `await` across the lock).
        let lease_id = {
            let mut table = self
                .upstream_lease_table
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            prune_expired_upstream_leases(&mut table, now_instant, &self.config);
            if table.account_lease_count(&account) >= upstream.max_concurrency.max(1) as usize {
                // C3.4: reject with a real retry-after estimate instead of the
                // old hard-coded 1s that had nothing to do with slot release.
                let retry_after = estimate_local_concurrency_retry_after_seconds(
                    &self.config,
                    &table,
                    &account,
                    now_instant,
                );
                // E5.3: count the safety-net rejection so the admin snapshot
                // can tell "the gate ever fired" from "the gate never fired".
                *table
                    .capacity_reject_total
                    .entry(upstream.id.clone())
                    .or_insert(0) += 1;
                return Err(UpstreamAdmissionError::new(
                    UpstreamAdmissionRejectionReason::LocalConcurrency,
                    "upstream request concurrency capacity is full".into(),
                    retry_after,
                ));
            }
            let lease_id = Uuid::new_v4().to_string();
            table.insert(
                account.clone(),
                lease_id.clone(),
                UpstreamLeaseRecord::new(
                    now_instant,
                    tokio::time::Duration::from_secs(
                        self.config.upstream_local_lease_ttl_seconds.max(1),
                    ),
                    UpstreamLeaseKind::Unary,
                ),
            );
            lease_id
        };
        state.minute_events.push_back(QuotaEvent {
            created_at: now,
            cost: request_cost,
        });
        state.five_hour_events.push_back(QuotaEvent {
            created_at: now,
            cost: request_cost,
        });
        Ok(UpstreamRequestLease {
            account,
            lease_id,
            release_state: Arc::new(AtomicU8::new(LEASE_RELEASE_ACTIVE)),
        })
    }

    pub async fn try_reserve_upstream_hedge(
        &self,
        upstream: &UpstreamConfig,
        model: &str,
    ) -> Result<UpstreamRequestLease, UpstreamAdmissionError> {
        let key_fingerprint = upstream_key_fingerprint(&upstream.id, &upstream.api_key);
        self.try_reserve_upstream_account_hedge(upstream, &key_fingerprint, model)
            .await
    }

    pub async fn try_reserve_upstream_account_hedge(
        &self,
        upstream: &UpstreamConfig,
        key_fingerprint: &str,
        _model: &str,
    ) -> Result<UpstreamRequestLease, UpstreamAdmissionError> {
        // Upstream model-weighted request costs were removed; every request
        // consumes a flat 1.0 quota unit.
        let request_cost = 1.0_f64;
        let account = AccountConcurrencyKey::new(upstream.id.clone(), key_fingerprint);

        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            let lease_id = Uuid::new_v4().to_string();
            coordinator
                .reserve_upstream_request(
                    upstream,
                    &account,
                    request_cost,
                    &Uuid::new_v4().to_string(),
                    &lease_id,
                    true,
                )
                .await?;
            return Ok(UpstreamRequestLease {
                account,
                lease_id,
                release_state: Arc::new(AtomicU8::new(LEASE_RELEASE_ACTIVE)),
            });
        }

        let mut runtime_state = self.upstream_runtime_state.lock().await;
        let state = runtime_state
            .entry(upstream.id.clone())
            .or_insert_with(UpstreamRuntimeState::default);
        let now_instant = tokio::time::Instant::now();
        let now = unix_seconds();
        prune_quota_events(&mut state.minute_events, now, 60);
        prune_quota_events(
            &mut state.five_hour_events,
            now,
            upstream.request_quota_window_seconds(),
        );

        // First concurrency gate (kept before the quota gates so a full
        // account reports LocalConcurrency just like the pre-C1 code did).
        {
            let table = self
                .upstream_lease_table
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if table.account_lease_count(&account) >= upstream.max_concurrency.max(1) as usize {
                let retry_after = estimate_local_concurrency_retry_after_seconds(
                    &self.config,
                    &table,
                    &account,
                    now_instant,
                );
                return Err(UpstreamAdmissionError::new(
                    UpstreamAdmissionRejectionReason::LocalConcurrency,
                    "upstream hedge concurrency capacity is full".into(),
                    retry_after,
                ));
            }
        }
        let minute_cost = quota_event_cost(&state.minute_events);
        if upstream.requests_per_minute > 0
            && minute_cost + request_cost > f64::from(upstream.requests_per_minute)
        {
            return Err(UpstreamAdmissionError::new(
                UpstreamAdmissionRejectionReason::HedgeMinuteQuota,
                "upstream hedge minute quota is exhausted".into(),
                1,
            ));
        }
        let window_cost = quota_event_cost(&state.five_hour_events);
        if upstream.request_quota_requests > 0
            && window_cost + request_cost > f64::from(upstream.request_quota_requests)
        {
            return Err(UpstreamAdmissionError::new(
                UpstreamAdmissionRejectionReason::HedgeWindowQuota,
                "upstream hedge request quota is exhausted".into(),
                1,
            ));
        }

        let lease_id = {
            let mut table = self
                .upstream_lease_table
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            prune_expired_upstream_leases(&mut table, now_instant, &self.config);
            if table.account_lease_count(&account) >= upstream.max_concurrency.max(1) as usize {
                let retry_after = estimate_local_concurrency_retry_after_seconds(
                    &self.config,
                    &table,
                    &account,
                    now_instant,
                );
                return Err(UpstreamAdmissionError::new(
                    UpstreamAdmissionRejectionReason::LocalConcurrency,
                    "upstream hedge concurrency capacity is full".into(),
                    retry_after,
                ));
            }
            let lease_id = Uuid::new_v4().to_string();
            table.insert(
                account.clone(),
                lease_id.clone(),
                UpstreamLeaseRecord::new(
                    now_instant,
                    tokio::time::Duration::from_secs(
                        self.config.upstream_local_lease_ttl_seconds.max(1),
                    ),
                    UpstreamLeaseKind::Unary,
                ),
            );
            lease_id
        };
        state.minute_events.push_back(QuotaEvent {
            created_at: now,
            cost: request_cost,
        });
        state.five_hour_events.push_back(QuotaEvent {
            created_at: now,
            cost: request_cost,
        });
        Ok(UpstreamRequestLease {
            account,
            lease_id,
            release_state: Arc::new(AtomicU8::new(LEASE_RELEASE_ACTIVE)),
        })
    }

    pub async fn upstream_runtime_snapshots(
        &self,
    ) -> Result<HashMap<String, UpstreamRuntimeSnapshot>, RuntimeCoordinationError> {
        let upstreams = {
            let state = self.inner.lock().await;
            state
                .upstreams
                .iter()
                .cloned()
                .collect::<Vec<UpstreamConfig>>()
        };

        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            let mut snapshots = HashMap::with_capacity(upstreams.len());
            for upstream in &upstreams {
                snapshots.insert(
                    upstream.id.clone(),
                    coordinator.upstream_snapshot(upstream).await?,
                );
            }
            return Ok(snapshots);
        }

        let upstream_windows = upstreams
            .into_iter()
            .map(|upstream| {
                let window_seconds = upstream.request_quota_window_seconds();
                (upstream.id, window_seconds)
            })
            .collect::<HashMap<String, u64>>();

        let mut runtime_state = self.upstream_runtime_state.lock().await;
        // E5.3: capture the route-health cooldown-skip counters under one
        // short lock *before* the runtime-state lock is held — the map
        // closure below cannot await, and the lease-table lock must never be
        // nested under the route-health lock.
        let cooldown_skipped_totals = {
            let route_health = self.route_health.lock().await;
            upstream_windows
                .keys()
                .map(|upstream_id| {
                    (
                        upstream_id.clone(),
                        route_health.cooldown_skipped_total(upstream_id),
                    )
                })
                .collect::<HashMap<String, u64>>()
        };
        let now = unix_seconds();
        Ok(runtime_state
            .iter_mut()
            .map(|(upstream_id, state)| {
                let request_quota_window_seconds = upstream_windows
                    .get(upstream_id)
                    .copied()
                    .unwrap_or(5 * 60 * 60);
                prune_quota_events(&mut state.minute_events, now, 60);
                prune_quota_events(
                    &mut state.five_hour_events,
                    now,
                    request_quota_window_seconds,
                );
                let mut table = self
                    .upstream_lease_table
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let lease_now = tokio::time::Instant::now();
                // C5.1: report the *pre-sweep* lease state.  The snapshot
                // doubles as a lazy sweep (prune below), so counting after it
                // would always read 0 — the operator needs to see that the
                // gate was full of stale/leaked leases *as of this snapshot*.
                let stale_after =
                    Duration::from_millis(self.config.upstream_lease_stale_after_ms.max(1));
                let stale_lease_count =
                    table.upstream_stale_lease_count(upstream_id, stale_after, lease_now);
                let oldest_lease_age_seconds =
                    table.upstream_oldest_lease_age_seconds(upstream_id, lease_now);
                prune_expired_upstream_leases(&mut table, lease_now, &self.config);
                let leaked_reclaimed_total = table
                    .leaked_reclaimed_total
                    .get(upstream_id)
                    .copied()
                    .unwrap_or(0);
                let stale_reclaimed_total = table
                    .stale_reclaimed_total
                    .get(upstream_id)
                    .copied()
                    .unwrap_or(0);
                // E5.3: process-local observables read before the lease-table
                // lock is released; the route-health counter is read under its
                // own lock afterwards (lock ordering: never nest route_health
                // inside the lease-table lock — no code path takes the reverse).
                let hold_p50_ms = table.upstream_hold_p50_ms(upstream_id);
                let hold_p95_ms = table.upstream_hold_p95_ms(upstream_id);
                let capacity_reject_total = table.capacity_reject_total(upstream_id);
                let in_flight = table
                    .account_lease_count_for_upstream(upstream_id)
                    .min(u32::MAX as usize) as u32;
                drop(table);
                let route_cooldown_skipped_total = cooldown_skipped_totals
                    .get(upstream_id)
                    .copied()
                    .unwrap_or(0);
                (
                    upstream_id.clone(),
                    UpstreamRuntimeSnapshot {
                        in_flight,
                        minute_cost: quota_event_cost(&state.minute_events),
                        five_hour_cost: quota_event_cost(&state.five_hour_events),
                        cooldown_until: state.cooldown_until,
                        leaked_reclaimed_total,
                        stale_reclaimed_total,
                        stale_lease_count: stale_lease_count.min(u32::MAX as usize) as u32,
                        oldest_lease_age_seconds,
                        queue_depth: self
                            .upstream_slot_waiter_count(upstream_id)
                            .min(u32::MAX as usize) as u32,
                        hold_p50_ms,
                        hold_p95_ms,
                        capacity_reject_total,
                        route_cooldown_skipped_total,
                    },
                )
            })
            .collect())
    }

    pub async fn release_upstream_request(
        &self,
        lease: UpstreamRequestLease,
    ) -> Result<(), RuntimeCoordinationError> {
        let Some(release_guard) = LeaseReleaseGuard::acquire(&lease.release_state)? else {
            return Ok(());
        };
        let result =
            if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
                coordinator
                    .release_upstream_lease(&lease.account, &lease.lease_id)
                    .await
            } else {
                // C1.2: local leases are removed synchronously from the
                // `std::sync::Mutex`-protected lease table.  No runtime.spawn,
                // no Tokio runtime dependency, no silent `try_lock` failure.
                let mut table = self
                    .upstream_lease_table
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                // E3: the release also samples the hold duration (release −
                // reserve) into the account ring for the Retry-After estimate.
                table.remove(&lease.account, &lease.lease_id, tokio::time::Instant::now());
                Ok(())
            };
        if result.is_ok() {
            release_guard.complete();
        }
        result
    }

    /// Extends an upstream request lease (P7): the long-stream counterpart of
    /// the downstream `renew_downstream_concurrency` hook.  The streaming body
    /// loop calls `UpstreamRequestReservation::renew_if_due` per chunk,
    /// throttled to half the configured local lease TTL, so a stream may run
    /// far longer than the TTL without its slot being reclaimed.  Idempotent:
    /// renewing a lease that was already released (or whose slot was reclaimed)
    /// is a no-op success, mirroring the Redis `lease_renew.lua` semantics.
    pub async fn renew_upstream_request(
        &self,
        lease: &UpstreamRequestLease,
    ) -> Result<(), RuntimeCoordinationError> {
        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            return coordinator
                .renew_upstream_request(&lease.account, &lease.lease_id)
                .await;
        }
        // C1.1: renew against the lease table (a purely synchronous critical
        // section).  Renewing a lease that was already released or reclaimed
        // is a no-op success, mirroring the Redis `lease_renew.lua` semantics.
        let now = tokio::time::Instant::now();
        let mut table = self
            .upstream_lease_table
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        prune_expired_upstream_leases(&mut table, now, &self.config);
        let ttl =
            tokio::time::Duration::from_secs(self.config.upstream_local_lease_ttl_seconds.max(1));
        table.renew(&lease.account, &lease.lease_id, now, ttl);
        Ok(())
    }

    /// Synchronous fallback for an `UpstreamRequestGuard` dropped outside a
    /// Tokio runtime (P7): the release task cannot be spawned (`spawn_release`
    /// gets `Handle::try_current` back `Err`), so instead of only logging and
    /// leaving the slot pinned until the TTL sweep, remove the lease right
    /// away.  Uses `try_lock` because this runs from `Drop`; when the mutex is
    /// contended the caller falls back to the lazy TTL reclamation.  Returns
    /// true when the lease was actually removed (a release that never ran).
    /// Redis-lease guards simply log (the Redis TTL self-heals natively).
    pub fn expire_upstream_request_lease_sync(&self, lease: &UpstreamRequestLease) -> bool {
        // C1.4: no more `try_lock` silent-failure path.  The lease table is a
        // plain `std::sync::Mutex` whose critical sections are purely
        // synchronous, so blocking briefly here (from `Drop`) is safe and the
        // release actually happens instead of being left for the lazy TTL
        // sweep.  Returns true when the lease was actually removed.
        let mut table = self
            .upstream_lease_table
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        table.remove(&lease.account, &lease.lease_id, tokio::time::Instant::now())
    }

    pub async fn mark_upstream_failure(&self, upstream_id: &str) -> io::Result<()> {
        self.mutate_persisted_state_io(|state| {
            if let Some(upstream) = Arc::make_mut(&mut state.upstreams)
                .iter_mut()
                .find(|upstream| upstream.id == upstream_id)
            {
                upstream.failure_count = upstream.failure_count.saturating_add(1);
            }
            Ok(())
        })
        .await
    }

    pub async fn mark_upstream_success(&self, upstream_id: &str) -> io::Result<()> {
        let persist_result = self
            .mutate_persisted_state_io(|state| {
                if let Some(upstream) = Arc::make_mut(&mut state.upstreams)
                    .iter_mut()
                    .find(|upstream| upstream.id == upstream_id)
                {
                    upstream.failure_count = 0;
                }
                Ok(())
            })
            .await;

        let cooldown_result =
            if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
                coordinator
                    .clear_upstream_cooldown(upstream_id)
                    .await
                    .map_err(io::Error::other)
            } else {
                let mut runtime_state = self.upstream_runtime_state.lock().await;
                if let Some(runtime) = runtime_state.get_mut(upstream_id) {
                    runtime.cooldown_until = 0;
                }
                Ok(())
            };

        cooldown_result?;
        persist_result
    }

    pub async fn mark_upstream_rate_limited(
        &self,
        upstream_id: &str,
        retry_after_seconds: u64,
    ) -> Result<(), RuntimeCoordinationError> {
        self.mark_upstream_cooldown(upstream_id, retry_after_seconds, "rate_limited")
            .await
    }

    pub async fn mark_upstream_concurrency_full(
        &self,
        upstream_id: &str,
        cooldown_ms: u64,
    ) -> Result<(), RuntimeCoordinationError> {
        let cooldown_seconds = cooldown_ms.saturating_add(999) / 1000;
        self.mark_upstream_cooldown(upstream_id, cooldown_seconds.max(1), "concurrency_full")
            .await
    }

    async fn mark_upstream_cooldown(
        &self,
        upstream_id: &str,
        cooldown_seconds: u64,
        feedback_type: &str,
    ) -> Result<(), RuntimeCoordinationError> {
        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            return coordinator
                .mark_upstream_cooldown(upstream_id, cooldown_seconds.max(1), feedback_type)
                .await;
        }
        let mut runtime_state = self.upstream_runtime_state.lock().await;
        let state = runtime_state
            .entry(upstream_id.to_string())
            .or_insert_with(UpstreamRuntimeState::default);
        let now = unix_seconds();
        let cooldown_until = now.saturating_add(cooldown_seconds.max(1));
        state.cooldown_until = state.cooldown_until.max(cooldown_until);
        state.last_feedback_type = Some(feedback_type.to_string());
        state.last_retry_after_seconds = Some(cooldown_seconds.max(1));
        Ok(())
    }

    pub async fn upstream_runtime_snapshots_with_feedback(
        &self,
    ) -> Result<HashMap<String, UpstreamRuntimeSnapshotWithFeedback>, RuntimeCoordinationError>
    {
        let upstreams = {
            let state = self.inner.lock().await;
            state
                .upstreams
                .iter()
                .cloned()
                .collect::<Vec<UpstreamConfig>>()
        };

        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            let mut snapshots = HashMap::with_capacity(upstreams.len());
            for upstream in &upstreams {
                snapshots.insert(
                    upstream.id.clone(),
                    coordinator
                        .upstream_snapshot_with_feedback(upstream)
                        .await?,
                );
            }
            return Ok(snapshots);
        }

        let upstream_windows = upstreams
            .into_iter()
            .map(|upstream| {
                let window_seconds = upstream.request_quota_window_seconds();
                (upstream.id, window_seconds)
            })
            .collect::<HashMap<String, u64>>();

        let mut runtime_state = self.upstream_runtime_state.lock().await;
        // E5.3: see `upstream_runtime_snapshots` — the counter snapshot is
        // taken here (before the lease table is accessed) so the map closure
        // below needs no await and no lock nesting.
        let cooldown_skipped_totals = {
            let route_health = self.route_health.lock().await;
            upstream_windows
                .keys()
                .map(|upstream_id| {
                    (
                        upstream_id.clone(),
                        route_health.cooldown_skipped_total(upstream_id),
                    )
                })
                .collect::<HashMap<String, u64>>()
        };
        let now = unix_seconds();

        Ok(upstream_windows
            .into_iter()
            .map(|(upstream_id, window_seconds)| {
                let state = runtime_state
                    .entry(upstream_id.clone())
                    .or_insert_with(UpstreamRuntimeState::default);

                prune_quota_events(&mut state.minute_events, now, 60);
                prune_quota_events(&mut state.five_hour_events, now, window_seconds);

                let minute_cost = state.minute_events.iter().map(|event| event.cost).sum();
                let five_hour_cost = state.five_hour_events.iter().map(|event| event.cost).sum();

                let mut table = self
                    .upstream_lease_table
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let lease_now = tokio::time::Instant::now();
                // C5.1: report the *pre-sweep* lease state (see the identical
                // note in `upstream_runtime_snapshots`).
                let stale_after =
                    Duration::from_millis(self.config.upstream_lease_stale_after_ms.max(1));
                let stale_lease_count =
                    table.upstream_stale_lease_count(&upstream_id, stale_after, lease_now);
                let oldest_lease_age_seconds =
                    table.upstream_oldest_lease_age_seconds(&upstream_id, lease_now);
                prune_expired_upstream_leases(&mut table, lease_now, &self.config);
                let leaked_reclaimed_total = table
                    .leaked_reclaimed_total
                    .get(&upstream_id)
                    .copied()
                    .unwrap_or(0);
                let stale_reclaimed_total = table
                    .stale_reclaimed_total
                    .get(&upstream_id)
                    .copied()
                    .unwrap_or(0);
                // E5.3: process-local observables (hold samples, gate rejects)
                // read from the lease table before its lock is dropped; the
                // route-health skip counter comes from the pre-taken snapshot.
                let hold_p50_ms = table.upstream_hold_p50_ms(&upstream_id);
                let hold_p95_ms = table.upstream_hold_p95_ms(&upstream_id);
                let capacity_reject_total = table.capacity_reject_total(&upstream_id);
                let route_cooldown_skipped_total = cooldown_skipped_totals
                    .get(&upstream_id)
                    .copied()
                    .unwrap_or(0);

                let snapshot = UpstreamRuntimeSnapshotWithFeedback {
                    in_flight: table
                        .account_lease_count_for_upstream(&upstream_id)
                        .min(u32::MAX as usize) as u32,
                    minute_cost,
                    five_hour_cost,
                    cooldown_until: state.cooldown_until,
                    cooldown_remaining: state.cooldown_until.saturating_sub(now),
                    last_feedback_type: state.last_feedback_type.clone(),
                    last_retry_after_seconds: state.last_retry_after_seconds,
                    leaked_reclaimed_total,
                    stale_reclaimed_total,
                    stale_lease_count: stale_lease_count.min(u32::MAX as usize) as u32,
                    oldest_lease_age_seconds,
                    queue_depth: self
                        .upstream_slot_waiter_count(&upstream_id)
                        .min(u32::MAX as usize) as u32,
                    hold_p50_ms,
                    hold_p95_ms,
                    capacity_reject_total,
                    route_cooldown_skipped_total,
                };

                (upstream_id, snapshot)
            })
            .collect())
    }

    pub async fn append_usage_log(&self, mut log: UsageLog) -> io::Result<()> {
        if log.id.is_empty() {
            log.id = Uuid::new_v4().to_string();
        }
        if log.created_at == 0 {
            log.created_at = unix_seconds();
        }

        self.record_downstream_usage_event(&log)
            .await
            .map_err(io::Error::other)?;

        {
            let mut pending = self.pending_usage_logs.lock().await;
            pending.push(log.clone());
        }

        self.schedule_usage_log_flush();
        Ok(())
    }

    async fn record_downstream_usage_event(
        &self,
        log: &UsageLog,
    ) -> Result<(), RuntimeCoordinationError> {
        // Rejected (429) and rolled-back (>=500) requests never consume quota;
        // keep the live cost window on the same counting rule as the replay
        // windows so live and replayed totals match.
        if log.status_code == 429 || log.status_code >= 500 {
            return Ok(());
        }
        let event = {
            let state = self.inner.lock().await;
            state
                .downstreams
                .iter()
                .find(|downstream| downstream.id == log.downstream_key_id)
                .filter(|downstream| {
                    downstream.rate_limit_enabled && downstream.cost_billing_mode()
                })
                .map(|downstream| {
                    // Prefer the billed amount recorded at request end so the
                    // live window matches the replay window (which reads
                    // total_cost_cents from usage logs).
                    let value = log.total_cost_cents.unwrap_or_else(|| {
                        downstream.cost_for_tokens(log.prompt_tokens, log.completion_tokens)
                    });
                    (downstream_token_retention_seconds(downstream), value)
                })
        };
        let Some((retention_seconds, event_value)) = event else {
            return Ok(());
        };
        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            return coordinator
                .record_downstream_tokens(
                    &log.downstream_key_id,
                    &format!("history:{}", log.id),
                    event_value,
                    retention_seconds,
                )
                .await;
        }
        let mut windows = self.downstream_token_windows.lock().await;
        windows
            .entry(log.downstream_key_id.clone())
            .or_insert_with(VecDeque::new)
            .push_back(DownstreamTokenEvent {
                created_at: log.created_at,
                tokens: event_value,
            });
        Ok(())
    }

    fn schedule_usage_log_flush(&self) {
        if self
            .usage_log_flush_running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            let app = self.clone();
            tokio::spawn(async move {
                app.flush_pending_usage_logs().await;
            });
        }
    }

    pub async fn flush_usage_logs_for_test(&self) -> io::Result<()> {
        loop {
            if self
                .usage_log_flush_running
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                let result = self.flush_pending_usage_logs_now().await;
                self.usage_log_flush_running.store(false, Ordering::Release);
                return result;
            }

            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn flush_pending_usage_logs(self) {
        loop {
            tokio::time::sleep(Duration::from_millis(50)).await;

            if let Err(error) = self.flush_pending_usage_logs_now().await {
                tracing::error!(error = %error, "failed to flush usage log batch");
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            }

            let pending_is_empty = {
                let pending = self.pending_usage_logs.lock().await;
                pending.is_empty()
            };

            if pending_is_empty {
                self.usage_log_flush_running.store(false, Ordering::Release);

                let restart = {
                    let pending = self.pending_usage_logs.lock().await;
                    !pending.is_empty()
                };
                if restart
                    && self
                        .usage_log_flush_running
                        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                {
                    continue;
                }
                return;
            }
        }
    }

    pub async fn prune_expired_usage_logs(&self) -> io::Result<u64> {
        let retention_days = self.config.usage_log_retention_days;
        if retention_days == 0 {
            return Ok(0);
        }
        let now = unix_seconds();
        let cutoff = now.saturating_sub(retention_days * 86400);

        // Delete from durable storage (file archives or postgres)
        if let Some(postgres) = &self.postgres {
            postgres.delete_usage_logs_before(cutoff).await?;
        } else {
            self.config_store.delete_usage_logs_before(cutoff).await?;
        }

        // Remove from in-memory archived logs
        let removed_archived = {
            let mut archived = self.archived_usage_logs.lock().await;
            let before = archived.len();
            archived.retain(|log| log.created_at >= cutoff);
            before - archived.len()
        };

        // Remove from in-memory state logs
        let removed_state = {
            let mut state = self.inner.lock().await;
            let before = state.usage_logs.len();
            state.usage_logs.retain(|log| log.created_at >= cutoff);
            before - state.usage_logs.len()
        };

        let total_removed = (removed_archived + removed_state) as u64;
        if total_removed > 0 {
            tracing::info!(
                cutoff,
                retention_days,
                removed = total_removed,
                "pruned expired usage logs"
            );
        }
        Ok(total_removed)
    }

    async fn flush_pending_usage_logs_now(&self) -> io::Result<()> {
        loop {
            let batch = {
                let pending = self.pending_usage_logs.lock().await;
                if pending.is_empty() {
                    Vec::new()
                } else {
                    pending.clone()
                }
            };

            if batch.is_empty() {
                return Ok(());
            }

            // Keep the batch visible to snapshots until durable storage and the
            // archived in-memory view both contain it.
            self.flush_usage_log_batch(&batch).await?;
            let mut pending = self.pending_usage_logs.lock().await;
            let persisted_count = batch.len().min(pending.len());
            pending.drain(..persisted_count);
        }
    }

    async fn flush_usage_log_batch(&self, batch: &[UsageLog]) -> io::Result<()> {
        if let Some(postgres) = &self.postgres {
            let _persist_guard = self.config_persist_lock.lock().await;
            postgres.append_usage_logs(batch).await?;
            let mut state = self.inner.lock().await;
            state.usage_logs.extend(batch.iter().cloned());
            return Ok(());
        }

        self.config_store.append_usage_logs(batch).await?;
        {
            let mut archived = self.archived_usage_logs.lock().await;
            archived.extend(batch.iter().cloned());
        }
        self.enforce_usage_log_archive_limit().await?;

        Ok(())
    }

    /// Atomically reserve both the request-quota slot and the concurrency
    /// lease for a downstream request. On Redis the two Lua operations are
    /// merged into a single round trip; on the local backend a rejected
    /// concurrency lease rolls back the request slot so admission stays
    /// all-or-nothing.
    pub async fn reserve_downstream_admission(
        &self,
        downstream: &DownstreamConfig,
        model: &str,
    ) -> Result<
        (DownstreamRequestReservation, DownstreamConcurrencyLease),
        DownstreamAdmissionRejection,
    > {
        if !downstream.rate_limit_enabled {
            // rate_limit_enabled=false skips the ENTIRE downstream gate, so both
            // per-model groups and the global max_concurrency are bypassed and
            // every request lands straight on the upstream local gate. Make the
            // operator aware that disabling this also disables the C7 groups.
            tracing::warn!(
                downstream_id = %downstream.id,
                "downstream rate limiting disabled: per-model concurrency groups and global max_concurrency are bypassed"
            );
            return Ok((
                DownstreamRequestReservation {
                    downstream_id: downstream.id.clone(),
                    event_id: None,
                },
                DownstreamConcurrencyLease {
                    downstream_id: downstream.id.clone(),
                    group_name: String::new(),
                    lease_id: None,
                    release_state: Arc::new(AtomicU8::new(LEASE_RELEASE_ACTIVE)),
                },
            ));
        }

        let case_insensitive = self.runtime_settings().model_case_insensitive_matching;
        let group = downstream.concurrency_group_for_model(model, case_insensitive);
        let group_name = group
            .as_ref()
            .map(|group| group.name.clone())
            .unwrap_or_default();

        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            let event_id = Uuid::new_v4().to_string();
            let lease_id = Uuid::new_v4().to_string();
            coordinator
                .reserve_downstream_admission(
                    downstream,
                    &event_id,
                    &lease_id,
                    &group_name,
                    group.as_ref().map(|group| group.max_concurrency),
                )
                .await?;
            return Ok((
                DownstreamRequestReservation {
                    downstream_id: downstream.id.clone(),
                    event_id: Some(event_id),
                },
                DownstreamConcurrencyLease {
                    downstream_id: downstream.id.clone(),
                    group_name,
                    lease_id: Some(lease_id),
                    release_state: Arc::new(AtomicU8::new(LEASE_RELEASE_ACTIVE)),
                },
            ));
        }

        let reservation = self.reserve_downstream_request(downstream).await?;
        match self
            .try_reserve_downstream_concurrency(downstream, model)
            .await
        {
            Ok(lease) => Ok((reservation, lease)),
            Err(rejection) => {
                // Best-effort rollback keeps local admission atomic: a rejected
                // concurrency lease must not consume a request-quota slot.
                let _ = self
                    .rollback_downstream_request_reservation(reservation)
                    .await;
                Err(rejection)
            }
        }
    }

    pub async fn reserve_downstream_request(
        &self,
        downstream: &DownstreamConfig,
    ) -> Result<DownstreamRequestReservation, DownstreamAdmissionRejection> {
        if !downstream.rate_limit_enabled {
            return Ok(DownstreamRequestReservation {
                downstream_id: downstream.id.clone(),
                event_id: None,
            });
        }

        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            let event_id = Uuid::new_v4().to_string();
            coordinator
                .reserve_downstream_request(downstream, &event_id)
                .await?;
            return Ok(DownstreamRequestReservation {
                downstream_id: downstream.id.clone(),
                event_id: Some(event_id),
            });
        }

        let mut windows = self.downstream_request_windows.lock().await;
        let window = windows
            .entry(downstream.id.clone())
            .or_insert_with(VecDeque::new);
        let now = unix_seconds();
        let request_quota_window_seconds = downstream
            .request_quota_window_hours
            .zip(downstream.request_quota_requests)
            .map(|(hours, _)| u64::from(hours.max(1)).saturating_mul(60 * 60));
        let retention_seconds = request_quota_window_seconds.unwrap_or(60).max(60);
        let window_start = now.saturating_sub(retention_seconds.saturating_sub(1));

        while let Some(event) = window.front() {
            if event.created_at < window_start {
                window.pop_front();
            } else {
                break;
            }
        }

        let minute_start = now.saturating_sub(59);
        let minute_count = window
            .iter()
            .filter(|event| event.created_at >= minute_start)
            .count();
        if minute_count >= downstream.per_minute_limit as usize {
            let oldest = window
                .iter()
                .find(|event| event.created_at >= minute_start)
                .map(|event| event.created_at)
                .unwrap_or(now);
            let retry_after = oldest.saturating_add(60).saturating_sub(now).max(1);
            return Err(DownstreamAdmissionRejection::PerMinuteLimitExceeded {
                retry_after_seconds: retry_after,
                limit: downstream.per_minute_limit,
                used: minute_count as u32,
            });
        }

        if !downstream.token_billing_mode() {
            if let Some(request_quota_window_seconds) = request_quota_window_seconds {
                let request_quota_requests = downstream.request_quota_requests.unwrap_or(0).max(1);
                let quota_start =
                    now.saturating_sub(request_quota_window_seconds.saturating_sub(1));
                let quota_count = window
                    .iter()
                    .filter(|event| event.created_at >= quota_start)
                    .count();
                if quota_count >= request_quota_requests as usize {
                    let oldest = window
                        .iter()
                        .find(|event| event.created_at >= quota_start)
                        .map(|event| event.created_at)
                        .unwrap_or(now);
                    let retry_after = oldest
                        .saturating_add(request_quota_window_seconds)
                        .saturating_sub(now)
                        .max(1);
                    return Err(DownstreamAdmissionRejection::RequestQuotaExceeded {
                        retry_after_seconds: retry_after,
                        limit: request_quota_requests,
                        used: quota_count as u32,
                        window_seconds: request_quota_window_seconds,
                    });
                }
            }
        }

        // Cost billing (token mode + prices + daily cost limit) enforces a
        // rolling 24h cost window. Raw token quotas (daily/monthly token
        // limits) are no longer enforced: only request-count and cost limits
        // apply.
        if let Some(daily_cost_limit) = downstream.daily_cost_limit() {
            let mut token_windows = self.downstream_token_windows.lock().await;
            let token_window = token_windows
                .entry(downstream.id.clone())
                .or_insert_with(VecDeque::new);
            let token_retention_seconds = downstream_token_retention_seconds(downstream);
            let token_window_start = now.saturating_sub(token_retention_seconds.saturating_sub(1));

            while let Some(event) = token_window.front() {
                if event.created_at < token_window_start {
                    token_window.pop_front();
                } else {
                    break;
                }
            }

            let daily_used = token_window
                .iter()
                .filter(|event| {
                    event.created_at
                        >= now
                            .saturating_sub(DOWNSTREAM_DAILY_TOKEN_WINDOW_SECONDS.saturating_sub(1))
                })
                .map(|event| event.tokens)
                .sum::<u64>();
            if daily_used >= daily_cost_limit.max(1) {
                let retry_after_seconds = downstream_token_retry_after_seconds(
                    token_window,
                    now,
                    DOWNSTREAM_DAILY_TOKEN_WINDOW_SECONDS,
                    daily_used
                        .saturating_add(1)
                        .saturating_sub(daily_cost_limit.max(1)),
                )
                .max(1);
                return Err(DownstreamAdmissionRejection::DailyCostQuotaExceeded {
                    retry_after_seconds,
                    limit: daily_cost_limit.max(1),
                    used: daily_used,
                });
            }
        }

        let event_id = Uuid::new_v4().to_string();
        window.push_back(DownstreamRequestEvent {
            event_id: event_id.clone(),
            created_at: now,
        });
        Ok(DownstreamRequestReservation {
            downstream_id: downstream.id.clone(),
            event_id: Some(event_id),
        })
    }

    pub async fn try_reserve_downstream_concurrency(
        &self,
        downstream: &DownstreamConfig,
        model: &str,
    ) -> Result<DownstreamConcurrencyLease, DownstreamAdmissionRejection> {
        if !downstream.rate_limit_enabled {
            return Ok(DownstreamConcurrencyLease {
                downstream_id: downstream.id.clone(),
                group_name: String::new(),
                lease_id: None,
                release_state: Arc::new(AtomicU8::new(LEASE_RELEASE_ACTIVE)),
            });
        }

        let case_insensitive = self.runtime_settings().model_case_insensitive_matching;
        let group = downstream.concurrency_group_for_model(model, case_insensitive);
        let group_name = group
            .as_ref()
            .map(|group| group.name.clone())
            .unwrap_or_default();

        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            let lease_id = Uuid::new_v4().to_string();
            coordinator
                .reserve_downstream_lease(
                    downstream,
                    &lease_id,
                    &group_name,
                    group.as_ref().map(|group| group.max_concurrency),
                )
                .await?;
            return Ok(DownstreamConcurrencyLease {
                downstream_id: downstream.id.clone(),
                group_name,
                lease_id: Some(lease_id),
                release_state: Arc::new(AtomicU8::new(LEASE_RELEASE_ACTIVE)),
            });
        }

        let lease_id = Uuid::new_v4().to_string();
        let mut runtime = self
            .downstream_runtime
            .lock()
            .expect("downstream runtime lock poisoned");
        let key = (downstream.id.clone(), group_name.clone());
        let group_cap = group.as_ref().map(|group| group.max_concurrency.max(1));
        // Group cap first: a burst on one model must not eat another model's
        // budget. The bucket is already per (downstream, group), so the count
        // is the group's own occupancy.
        if let Some(group_cap) = group_cap {
            let current = runtime.entry(key.clone()).or_default();
            current.prune_expired(unix_millis());
            if current.admitted.len() >= group_cap as usize {
                return Err(DownstreamAdmissionRejection::ConcurrencyLimitExceeded {
                    retry_after_seconds: 1,
                    limit: group_cap,
                    group: Some(group_name.clone()),
                });
            }
        }
        // Global cap as a downstream-wide backstop (across every group). It
        // bounds the gateway-process resources a single downstream key may
        // consume; a group list whose caps sum above it is legal overbooking
        // and the global cap keeps the total bounded.
        if downstream_total_admitted(&runtime, &downstream.id)
            >= downstream.max_concurrency.max(1) as usize
        {
            return Err(DownstreamAdmissionRejection::ConcurrencyLimitExceeded {
                retry_after_seconds: 1,
                limit: downstream.max_concurrency.max(1),
                group: (!group_name.is_empty()).then(|| group_name.clone()),
            });
        }
        let lease_duration_ms = self
            .config
            .downstream_lease_ttl_seconds
            .saturating_mul(1_000)
            .max(60_000);
        let current = runtime.entry(key).or_default();
        current.prune_expired(unix_millis());
        current.admitted.insert(
            lease_id.clone(),
            unix_millis().saturating_add(lease_duration_ms),
        );
        Ok(DownstreamConcurrencyLease {
            downstream_id: downstream.id.clone(),
            group_name,
            lease_id: Some(lease_id),
            release_state: Arc::new(AtomicU8::new(LEASE_RELEASE_ACTIVE)),
        })
    }

    pub async fn release_downstream_concurrency(
        &self,
        lease: DownstreamConcurrencyLease,
    ) -> Result<(), RuntimeCoordinationError> {
        let Some(release_guard) = LeaseReleaseGuard::acquire(&lease.release_state)? else {
            return Ok(());
        };
        let result = if let Some(lease_id) = lease.lease_id {
            if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
                coordinator
                    .release_downstream_lease(&lease.downstream_id, &lease_id, &lease.group_name)
                    .await
            } else {
                let mut runtime = self
                    .downstream_runtime
                    .lock()
                    .expect("downstream runtime lock poisoned");
                let key = (lease.downstream_id.clone(), lease.group_name.clone());
                let remove_entry = runtime.get_mut(&key).is_some_and(|state| {
                    state.admitted.remove(&lease_id);
                    state.waiting.remove(&lease_id);
                    state.is_empty()
                });
                if remove_entry {
                    runtime.remove(&key);
                }
                Ok(())
            }
        } else {
            Ok(())
        };
        if result.is_ok() {
            release_guard.complete();
        }
        result
    }

    /// C1.2 (downstream counterpart): synchronous downstream-lease release for
    /// the local backend.  Mirrors `expire_upstream_request_lease_sync`: the
    /// local `downstream_runtime` sits behind a plain `std::sync::Mutex` whose
    /// critical section is purely synchronous, so a `Drop` (or a guard whose
    /// spawned release task may never be polled) can release the slot right
    /// here instead of depending on `runtime.spawn` scheduling.  Redis stays on
    /// the async path (its lease TTL self-heals).  Idempotent: a lease already
    /// released reports false; a missing slot reports false too.
    pub fn expire_downstream_request_lease_sync(&self, lease: &DownstreamConcurrencyLease) -> bool {
        let Some(lease_id) = lease.lease_id.as_deref() else {
            return false;
        };
        let Some(release_guard) = LeaseReleaseGuard::acquire(&lease.release_state)
            .ok()
            .flatten()
        else {
            return false;
        };
        let mut runtime = self
            .downstream_runtime
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let key = (lease.downstream_id.clone(), lease.group_name.clone());
        let remove_entry = runtime.get_mut(&key).is_some_and(|state| {
            state.admitted.remove(lease_id);
            state.waiting.remove(lease_id);
            state.is_empty()
        });
        if remove_entry {
            runtime.remove(&key);
        }
        release_guard.complete();
        remove_entry
    }

    pub async fn renew_downstream_concurrency(
        &self,
        lease: &DownstreamConcurrencyLease,
    ) -> Result<(), RuntimeCoordinationError> {
        let Some(lease_id) = lease.lease_id.as_deref() else {
            return Ok(());
        };
        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            return coordinator
                .renew_downstream_lease(&lease.downstream_id, lease_id, &lease.group_name)
                .await;
        }
        let mut runtime = self
            .downstream_runtime
            .lock()
            .expect("downstream runtime lock poisoned");
        let key = (lease.downstream_id.clone(), lease.group_name.clone());
        let Some(state) = runtime.get_mut(&key) else {
            // The lease was already released; renewal is a no-op.
            return Ok(());
        };
        if state.admitted.contains_key(lease_id) {
            let lease_duration_ms = self
                .config
                .downstream_lease_ttl_seconds
                .saturating_mul(1_000)
                .max(60_000);
            state.admitted.insert(
                lease_id.to_string(),
                unix_millis().saturating_add(lease_duration_ms),
            );
        }
        Ok(())
    }

    pub async fn mark_downstream_waiting(
        &self,
        lease: &DownstreamConcurrencyLease,
    ) -> Result<(), RuntimeCoordinationError> {
        let Some(lease_id) = lease.lease_id.as_deref() else {
            return Ok(());
        };
        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            return coordinator
                .mark_downstream_waiting(&lease.downstream_id, lease_id, &lease.group_name)
                .await;
        }
        let mut runtime = self
            .downstream_runtime
            .lock()
            .expect("downstream runtime lock poisoned");
        let key = (lease.downstream_id.clone(), lease.group_name.clone());
        let state = runtime.get_mut(&key).ok_or(RuntimeCoordinationError)?;
        state.prune_expired(unix_millis());
        if !state.admitted.contains_key(lease_id) {
            return Err(RuntimeCoordinationError);
        }
        state.waiting.insert(lease_id.to_string());
        Ok(())
    }

    pub async fn unmark_downstream_waiting(
        &self,
        lease: &DownstreamConcurrencyLease,
    ) -> Result<(), RuntimeCoordinationError> {
        let Some(lease_id) = lease.lease_id.as_deref() else {
            return Ok(());
        };
        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            return coordinator
                .unmark_downstream_waiting(&lease.downstream_id, lease_id, &lease.group_name)
                .await;
        }
        let mut runtime = self
            .downstream_runtime
            .lock()
            .expect("downstream runtime lock poisoned");
        let key = (lease.downstream_id.clone(), lease.group_name.clone());
        if let Some(state) = runtime.get_mut(&key) {
            state.waiting.remove(lease_id);
        }
        Ok(())
    }

    pub async fn downstream_runtime_snapshot(
        &self,
        downstream: &DownstreamConfig,
    ) -> Result<DownstreamRuntimeCounts, RuntimeCoordinationError> {
        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            let group_names: Vec<String> = downstream
                .model_concurrency_groups
                .iter()
                .map(|group| group.name.clone())
                .collect();
            return coordinator
                .downstream_runtime_snapshot(&downstream.id, &group_names)
                .await;
        }
        let mut runtime = self
            .downstream_runtime
            .lock()
            .expect("downstream runtime lock poisoned");
        let mut admitted = 0u32;
        let mut waiting_upstream = 0u32;
        for ((_, _), state) in runtime
            .iter_mut()
            .filter(|((id, _), _)| id == downstream.id.as_str())
        {
            if !state.waiting.is_empty() || !state.admitted.is_empty() {
                state.prune_expired(unix_millis());
            }
            admitted =
                admitted.saturating_add(u32::try_from(state.admitted.len()).unwrap_or(u32::MAX));
            waiting_upstream = waiting_upstream
                .saturating_add(u32::try_from(state.waiting.len()).unwrap_or(u32::MAX));
        }
        let running = admitted
            .checked_sub(waiting_upstream)
            .ok_or(RuntimeCoordinationError)?;
        Ok(DownstreamRuntimeCounts {
            admitted,
            waiting_upstream,
            running,
        })
    }

    pub async fn all_downstream_runtime_snapshots(
        &self,
    ) -> Result<Vec<(String, DownstreamConcurrencySnapshot)>, RuntimeCoordinationError> {
        let routing = self.routing_snapshot().await;
        let now = unix_seconds();

        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            let mut snapshots = Vec::with_capacity(routing.downstreams.len());
            for downstream in routing.downstreams.iter() {
                let group_names: Vec<String> = downstream
                    .model_concurrency_groups
                    .iter()
                    .map(|group| group.name.clone())
                    .collect();
                let counts = coordinator
                    .downstream_runtime_snapshot(&downstream.id, &group_names)
                    .await?;
                snapshots.push((
                    downstream.id.clone(),
                    DownstreamConcurrencySnapshot::from_counts(
                        counts.admitted,
                        counts.waiting_upstream,
                        downstream.max_concurrency,
                        now,
                    ),
                ));
            }
            return Ok(snapshots);
        }

        let mut runtime = self
            .downstream_runtime
            .lock()
            .expect("downstream runtime lock poisoned");
        let now_ms = unix_millis();
        for state in runtime.values_mut() {
            state.prune_expired(now_ms);
        }
        Ok(routing
            .downstreams
            .iter()
            .map(|downstream| {
                let (admitted, waiting) = runtime
                    .iter()
                    .filter(|((id, _), _)| id == downstream.id.as_str())
                    .fold((0u32, 0u32), |(admitted, waiting), (_, state)| {
                        (
                            admitted.saturating_add(
                                u32::try_from(state.admitted.len()).unwrap_or(u32::MAX),
                            ),
                            waiting.saturating_add(
                                u32::try_from(state.waiting.len()).unwrap_or(u32::MAX),
                            ),
                        )
                    });
                (
                    downstream.id.clone(),
                    DownstreamConcurrencySnapshot::from_counts(
                        admitted,
                        waiting,
                        downstream.max_concurrency,
                        now,
                    ),
                )
            })
            .collect())
    }

    pub async fn clear_downstream_runtime(
        &self,
        downstream_id: &str,
        group_names: &[String],
    ) -> Result<(), RuntimeCoordinationError> {
        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            return coordinator
                .clear_downstream(downstream_id, group_names)
                .await;
        }
        self.downstream_request_windows
            .lock()
            .await
            .remove(downstream_id);
        self.downstream_token_windows
            .lock()
            .await
            .remove(downstream_id);
        self.downstream_runtime
            .lock()
            .expect("downstream runtime lock poisoned")
            .retain(|(id, _), _| id != downstream_id);
        Ok(())
    }

    pub async fn rollback_downstream_request_reservation(
        &self,
        reservation: DownstreamRequestReservation,
    ) -> Result<(), RuntimeCoordinationError> {
        let Some(event_id) = reservation.event_id else {
            return Ok(());
        };
        if let RuntimeCoordinationBackend::Redis(coordinator) = &self.runtime_coordination {
            return coordinator
                .rollback_downstream_request(&reservation.downstream_id, &event_id)
                .await;
        }
        let mut windows = self.downstream_request_windows.lock().await;
        if let Some(window) = windows.get_mut(&reservation.downstream_id) {
            window.retain(|event| event.event_id != event_id);
            if window.is_empty() {
                windows.remove(&reservation.downstream_id);
            }
        }
        Ok(())
    }

    fn routing_model_key(&self, model: &str) -> String {
        if self.runtime_settings().model_case_insensitive_matching {
            canonical_model_id(model)
        } else {
            model.trim().to_string()
        }
    }

    fn routing_affinity_key(&self, downstream_id: &str, normalized_model: &str) -> String {
        format!(
            "{}::{}",
            downstream_id,
            self.routing_model_key(normalized_model)
        )
    }

    pub fn get_affinity_upstream(
        &self,
        downstream_id: &str,
        normalized_model: &str,
    ) -> Option<String> {
        let key = self.routing_affinity_key(downstream_id, normalized_model);
        let mut affinity = self
            .routing_affinity
            .lock()
            .expect("routing affinity lock poisoned");
        let now = unix_seconds();
        let entry = affinity.get(&key)?.clone();
        if entry.expires_at > now {
            return Some(entry.upstream_id);
        }
        affinity.remove(&key);
        None
    }

    pub fn set_affinity_upstream(
        &self,
        downstream_id: &str,
        normalized_model: &str,
        upstream_id: &str,
        ttl_seconds: u64,
    ) {
        let key = self.routing_affinity_key(downstream_id, normalized_model);
        let expires_at = unix_seconds().saturating_add(ttl_seconds.max(1));
        let mut affinity = self
            .routing_affinity
            .lock()
            .expect("routing affinity lock poisoned");
        affinity.insert(
            key,
            RoutingAffinityEntry {
                upstream_id: upstream_id.to_string(),
                expires_at,
            },
        );
    }

    pub fn clear_affinity_upstream(&self, downstream_id: &str, normalized_model: &str) {
        let key = self.routing_affinity_key(downstream_id, normalized_model);
        let mut affinity = self
            .routing_affinity
            .lock()
            .expect("routing affinity lock poisoned");
        affinity.remove(&key);
    }

    fn routing_tie_breaker_key(
        &self,
        downstream_id: &str,
        normalized_model: &str,
        protocol: UpstreamProtocol,
    ) -> String {
        format!(
            "{}::{}::{protocol:?}",
            downstream_id,
            self.routing_model_key(normalized_model)
        )
    }

    pub fn next_routing_tie_breaker(
        &self,
        downstream_id: &str,
        normalized_model: &str,
        protocol: UpstreamProtocol,
    ) -> u64 {
        let key = self.routing_tie_breaker_key(downstream_id, normalized_model, protocol);
        let mut tie_breakers = self
            .routing_tie_breakers
            .lock()
            .expect("routing tie breaker lock poisoned");
        let entry = tie_breakers.entry(key).or_insert(0);
        let current = *entry;
        *entry = entry.saturating_add(1);
        current
    }

    pub async fn insert_upstream(&self, mut upstream: UpstreamConfig) -> io::Result<()> {
        upstream.normalize_for_storage();
        if let Err(error) = upstream.validate_configuration() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, error));
        }
        let alias_registry = self.model_alias_registry();
        if let Err(error) = upstream.validate_model_mappings_against_aliases(&alias_registry) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, error));
        }

        let queue_candidate = upstream.clone();
        self.mutate_persisted_state_io(|state| {
            if let Some(existing) = state
                .upstreams
                .iter()
                .find(|existing| existing.id == upstream.id)
            {
                let mut candidate_for_comparison = upstream.clone();
                candidate_for_comparison.failure_count = existing.failure_count;
                if existing == &candidate_for_comparison {
                    return Ok(());
                }
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!(
                        "upstream id \"{}\" already exists with different configuration",
                        upstream.id
                    ),
                ));
            }
            Arc::make_mut(&mut state.upstreams).push(upstream);
            Ok(())
        })
        .await?;
        let current_upstreams = self.routing_snapshot().await.upstreams;
        self.reconcile_route_health(&current_upstreams)
            .await
            .map_err(io::Error::other)?;
        let jobs = self.capability_probe_jobs_for_upstream(&queue_candidate);
        self.submit_capability_probe_jobs(jobs, ProbeReason::ConfigurationChanged)
            .await
    }

    pub async fn update_upstream(
        &self,
        upstream_id: &str,
        upstream: UpstreamConfig,
    ) -> io::Result<bool> {
        let updated = self
            .mutate_persisted_state_io(|state| {
                let Some(existing) = Arc::make_mut(&mut state.upstreams)
                    .iter_mut()
                    .find(|upstream| upstream.id == upstream_id)
                else {
                    return Ok(false);
                };

                let mut upstream = upstream;
                upstream.id = upstream_id.to_string();
                upstream.normalize_for_storage();
                *existing = upstream;
                Ok(true)
            })
            .await?;
        if updated {
            let current_upstreams = self.routing_snapshot().await.upstreams;
            self.reconcile_route_health(&current_upstreams)
                .await
                .map_err(io::Error::other)?;
            if let Some(upstream) = self
                .routing_snapshot()
                .await
                .upstreams
                .iter()
                .find(|upstream| upstream.id == upstream_id)
                .cloned()
            {
                let jobs = self.capability_probe_jobs_for_upstream(&upstream);
                self.submit_capability_probe_jobs(jobs, ProbeReason::ConfigurationChanged)
                    .await?;
            }
        }
        Ok(updated)
    }

    pub async fn remove_upstream(&self, upstream_id: &str) -> io::Result<bool> {
        let removed = self
            .mutate_persisted_state_io(|state| {
                let original_len = state.upstreams.len();
                Arc::make_mut(&mut state.upstreams).retain(|upstream| upstream.id != upstream_id);
                Ok(state.upstreams.len() != original_len)
            })
            .await?;

        if removed {
            self.upstream_runtime_state.lock().await.remove(upstream_id);
            self.upstream_lease_table
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove_upstream(upstream_id);
            self.delete_dialect_profiles_for_upstream(upstream_id)
                .await?;
            let current_upstreams = self.routing_snapshot().await.upstreams;
            self.reconcile_route_health(&current_upstreams)
                .await
                .map_err(io::Error::other)?;
        }

        Ok(removed)
    }

    pub async fn insert_downstream(&self, downstream: DownstreamConfig) -> io::Result<()> {
        self.mutate_persisted_state_io(|state| {
            Arc::make_mut(&mut state.downstreams).push(downstream);
            Ok(())
        })
        .await
    }

    pub async fn update_downstream(
        &self,
        downstream_id: &str,
        downstream: DownstreamConfig,
    ) -> io::Result<bool> {
        self.mutate_persisted_state_io(|state| {
            let Some(existing) = Arc::make_mut(&mut state.downstreams)
                .iter_mut()
                .find(|downstream| downstream.id == downstream_id)
            else {
                return Ok(false);
            };

            let mut downstream = downstream;
            downstream.id = downstream_id.to_string();
            *existing = downstream;
            Ok(true)
        })
        .await
    }

    pub async fn remove_downstream(&self, downstream_id: &str) -> io::Result<bool> {
        let redis_runtime = matches!(
            &self.runtime_coordination,
            RuntimeCoordinationBackend::Redis(_)
        );
        if redis_runtime {
            let (exists, groups) = {
                let state = self.inner.lock().await;
                let found = state
                    .downstreams
                    .iter()
                    .find(|downstream| downstream.id == downstream_id);
                (
                    found.is_some(),
                    found
                        .map(|downstream| {
                            downstream
                                .model_concurrency_groups
                                .iter()
                                .map(|group| group.name.clone())
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default(),
                )
            };
            if !exists {
                return Ok(false);
            }
            self.clear_downstream_runtime(downstream_id, &groups)
                .await
                .map_err(io::Error::other)?;
        }
        let removed = self
            .mutate_persisted_state_io(|state| {
                let original_len = state.downstreams.len();
                Arc::make_mut(&mut state.downstreams)
                    .retain(|downstream| downstream.id != downstream_id);
                Ok(state.downstreams.len() != original_len)
            })
            .await?;
        if removed && !redis_runtime {
            self.clear_downstream_runtime(downstream_id, &[])
                .await
                .map_err(io::Error::other)?;
        }
        Ok(removed)
    }

    pub async fn set_downstream_active(
        &self,
        downstream_id: &str,
        active: bool,
    ) -> io::Result<bool> {
        self.mutate_persisted_state_io(|state| {
            let Some(downstream) = Arc::make_mut(&mut state.downstreams)
                .iter_mut()
                .find(|downstream| downstream.id == downstream_id)
            else {
                return Ok(false);
            };
            downstream.active = active;
            Ok(true)
        })
        .await
    }

    pub async fn set_upstream_active(&self, upstream_id: &str, active: bool) -> io::Result<bool> {
        let updated = self
            .mutate_persisted_state_io(|state| {
                let Some(upstream) = Arc::make_mut(&mut state.upstreams)
                    .iter_mut()
                    .find(|upstream| upstream.id == upstream_id)
                else {
                    return Ok(false);
                };
                upstream.active = active;
                Ok(true)
            })
            .await?;
        if updated {
            let current_upstreams = self.routing_snapshot().await.upstreams;
            self.reconcile_route_health(&current_upstreams)
                .await
                .map_err(io::Error::other)?;
        }
        Ok(updated)
    }

    pub async fn upstreams(&self) -> Vec<UpstreamConfig> {
        Arc::unwrap_or_clone(self.routing_snapshot().await.upstreams)
    }

    pub async fn downstreams(&self) -> Vec<DownstreamConfig> {
        Arc::unwrap_or_clone(self.routing_snapshot().await.downstreams)
    }

    pub async fn usage_logs(&self) -> Vec<UsageLog> {
        self.snapshot().await.usage_logs
    }

    pub async fn reconcile_dialect_profiles(&self, now: u64) -> io::Result<Vec<ProbeJob>> {
        let runtime_settings = self.runtime_settings();
        let routing = self.routing_snapshot().await;
        let snapshot = self.capability_snapshot();
        let stale_profile_keys = snapshot
            .profiles
            .keys()
            .filter(|key| !dialect_profile_key_is_routable(&routing, key))
            .cloned()
            .collect::<Vec<_>>();
        for key in stale_profile_keys {
            self.delete_dialect_profile(&key).await?;
        }

        let snapshot = self.capability_snapshot();
        if !runtime_settings.automatic_capability_probes_enabled
            || !snapshot.configuration.source().probe.enabled
        {
            return Ok(Vec::new());
        }
        let refresh_interval_seconds = snapshot
            .configuration
            .source()
            .probe
            .refresh_interval_seconds;
        let mut jobs = Vec::<ProbeJob>::new();
        let mut queued = HashMap::<DialectProfileKey, usize>::new();
        for upstream in routing.upstreams.iter().filter(|upstream| upstream.active) {
            for exposed in upstream.effective_downstream_models() {
                let exposed_to_downstream = routing.downstreams.iter().any(|downstream| {
                    downstream.active
                        && (downstream.model_allowlist.is_empty()
                            || portal_model_is_allowed(&downstream.model_allowlist, &exposed))
                });
                if !exposed_to_downstream {
                    continue;
                }
                let Some(runtime) = upstream.resolved_model_name(&exposed) else {
                    continue;
                };
                for api_key in upstream.keys_for_model(&runtime) {
                    let key_fingerprint = upstream_key_fingerprint(&upstream.id, &api_key);
                    for protocol in upstream.supported_protocols() {
                        let key = DialectProfileKey::for_key(
                            upstream.id.clone(),
                            key_fingerprint.clone(),
                            runtime.clone(),
                            protocol.into(),
                        );
                        let fingerprint = self.route_configuration_fingerprint(
                            upstream,
                            &key_fingerprint,
                            &exposed,
                            &runtime,
                            protocol,
                        )?;
                        let current = snapshot.profiles.get(&key);
                        if !current
                            .map(|profile| {
                                capability_profile_is_current_or_retry_pending(
                                    profile,
                                    &fingerprint,
                                    now,
                                    refresh_interval_seconds,
                                )
                            })
                            .unwrap_or(false)
                        {
                            if let Some(index) = queued.get(&key).copied() {
                                jobs[index].exposed_model_slugs.insert(exposed.clone());
                            } else {
                                let Some(job) =
                                    Self::build_capability_probe_job_for_key_with_snapshot(
                                        &snapshot,
                                        upstream,
                                        &key_fingerprint,
                                        &exposed,
                                        &runtime,
                                        protocol,
                                        ProbeReason::ConfigurationChanged,
                                        ProbeMode::Full,
                                    )?
                                else {
                                    continue;
                                };
                                queued.insert(key.clone(), jobs.len());
                                jobs.push(job);
                            }
                        }
                    }
                }
            }
        }
        Ok(jobs)
    }

    pub async fn available_models_for_downstream(&self, secret: &str) -> Vec<String> {
        let Some(downstream) = self.downstream_for_secret(secret).await else {
            return Vec::new();
        };
        let snapshot = self.routing_snapshot().await;
        let case_insensitive = self.runtime_settings().model_case_insensitive_matching;
        let alias_registry = self.model_alias_registry();

        let mut seen = HashSet::new();
        let mut models = Vec::new();
        for upstream in snapshot.upstreams.iter().filter(|upstream| upstream.active) {
            for entry in upstream.effective_downstream_models_detailed() {
                let model = entry.model.as_str();
                if downstream.model_allowlist.is_empty()
                    || portal_model_is_allowed(&downstream.model_allowlist, &model)
                {
                    // Admin-picked per-upstream mapping labels are exposed
                    // verbatim (the operator typed them); everything else
                    // resolves through alias rules first, then falls back to
                    // B1 case-folding for display.
                    let display_name = if entry.from_mapping {
                        model.trim().to_string()
                    } else if let Some(canonical) = alias_registry.resolve_alias(&model) {
                        canonical.to_string()
                    } else if case_insensitive {
                        canonical_model_id(&model)
                    } else {
                        model.trim().to_string()
                    };

                    let key = if case_insensitive {
                        canonical_model_id(&display_name)
                    } else {
                        display_name.clone()
                    };

                    if key.is_empty() || !seen.insert(key) {
                        continue;
                    }
                    models.push(display_name);
                }
            }
        }

        models.sort();
        models
    }

    /// Union of models visible to at least one active downstream: every
    /// route model that falls inside a downstream allowlist (downstreams
    /// with an empty allowlist see every route model). Matches the
    /// per-downstream semantics of `available_models_for_downstream`.
    pub async fn downstream_visible_models(&self) -> Vec<String> {
        let snapshot = self.routing_snapshot().await;
        let case_insensitive = self.runtime_settings().model_case_insensitive_matching;
        let alias_registry = self.model_alias_registry();

        let mut seen = HashSet::new();
        let mut models = Vec::new();
        for upstream in snapshot.upstreams.iter().filter(|upstream| upstream.active) {
            for entry in upstream.effective_downstream_models_detailed() {
                let model = entry.model.as_str();
                if snapshot.downstreams.iter().any(|downstream| {
                    downstream.active
                        && (downstream.model_allowlist.is_empty()
                            || portal_model_is_allowed(&downstream.model_allowlist, &model))
                }) {
                    // Admin-picked per-upstream mapping labels are exposed
                    // verbatim (see available_models_for_downstream).
                    let display_name = if entry.from_mapping {
                        model.trim().to_string()
                    } else if let Some(canonical) = alias_registry.resolve_alias(&model) {
                        canonical.to_string()
                    } else if case_insensitive {
                        canonical_model_id(&model)
                    } else {
                        model.trim().to_string()
                    };

                    let key = if case_insensitive {
                        canonical_model_id(&display_name)
                    } else {
                        display_name.clone()
                    };

                    if key.is_empty() || !seen.insert(key) {
                        continue;
                    }
                    models.push(display_name);
                }
            }
        }
        models.sort();
        models
    }

    async fn initialize_capability_snapshot_from_store(
        &self,
        mut capability_state: CapabilityStateDocument,
    ) -> io::Result<()> {
        let _guard = self.capability_update_lock.lock().await;
        let migrated =
            crate::capabilities::sanitize_sensitive_urls(&mut capability_state.configuration);
        let bootstrapped = self.config.capability_policy_bootstrap_on_zero
            && capability_state.configuration.revision == 0;
        let mut merged_builtin_entries = Vec::new();
        if bootstrapped {
            capability_state.configuration =
                crate::capabilities::deployment_capability_configuration()
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        } else if capability_state.configuration.revision > 0 {
            // Append-only upgrade for operator-managed configurations: new
            // builtin (domestic-*) entries from the embedded template are
            // merged in without touching operator entries, so upgrades reach
            // deployments whose revision never returns to zero.
            let template = crate::capabilities::deployment_capability_configuration()
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            if template.builtin_policy_version
                > capability_state.configuration.builtin_policy_version
            {
                merged_builtin_entries = crate::capabilities::merge_builtin_policy_entries(
                    &mut capability_state.configuration,
                    &template,
                );
                if !merged_builtin_entries.is_empty() {
                    capability_state.configuration.revision += 1;
                    capability_state.configuration.builtin_policy_version =
                        template.builtin_policy_version;
                }
            }
        }
        if !merged_builtin_entries.is_empty() {
            tracing::info!(
                capability_policy_merged_revision = capability_state.configuration.revision,
                capability_policy_builtin_version = capability_state.configuration.builtin_policy_version,
                merged_entries = ?merged_builtin_entries,
                "merged new builtin capability policy entries into stored configuration"
            );
        }
        let compiled = Arc::new(
            capability_state
                .configuration
                .compile()
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
        );
        if bootstrapped || migrated || !merged_builtin_entries.is_empty() {
            self.config_store
                .persist_capability_configuration(&capability_state.configuration)
                .await?;
        }

        let routing = self.routing_snapshot().await;
        let fingerprint_snapshot = CapabilityRuntimeSnapshot {
            configuration: compiled.clone(),
            profiles: BTreeMap::new(),
        };
        let original_keys = capability_state
            .profiles
            .keys()
            .cloned()
            .collect::<HashSet<_>>();
        let mut reconciled = BTreeMap::new();
        for (stored_key, mut profile) in std::mem::take(&mut capability_state.profiles) {
            let Some(upstream) = routing
                .upstreams
                .iter()
                .find(|upstream| upstream.id == stored_key.upstream_id)
            else {
                self.config_store
                    .delete_dialect_profile(&stored_key)
                    .await?;
                continue;
            };
            let Some(protocol) = upstream_protocol_from_wire(stored_key.protocol) else {
                self.config_store
                    .delete_dialect_profile(&stored_key)
                    .await?;
                continue;
            };
            if !upstream.active || !upstream.supports_protocol(protocol) {
                self.config_store
                    .delete_dialect_profile(&stored_key)
                    .await?;
                continue;
            }

            let mut exposed_models = upstream
                .effective_downstream_models()
                .into_iter()
                .filter(|exposed| {
                    upstream.resolved_model_name(exposed).as_deref()
                        == Some(stored_key.runtime_model_slug.as_str())
                })
                .collect::<Vec<_>>();
            if exposed_models.is_empty()
                && upstream
                    .resolved_model_name(&stored_key.runtime_model_slug)
                    .as_deref()
                    == Some(stored_key.runtime_model_slug.as_str())
            {
                exposed_models.push(stored_key.runtime_model_slug.clone());
            }
            let route_keys = upstream.keys_for_model(&stored_key.runtime_model_slug);
            if exposed_models.is_empty() || route_keys.is_empty() {
                self.config_store
                    .delete_dialect_profile(&stored_key)
                    .await?;
                continue;
            }
            let key_fingerprints = route_keys
                .iter()
                .map(|api_key| upstream_key_fingerprint(&upstream.id, api_key))
                .collect::<BTreeSet<_>>();

            if !stored_key.key_fingerprint.is_empty() {
                if key_fingerprints.contains(&stored_key.key_fingerprint) {
                    profile.key = stored_key.clone();
                    reconciled.insert(stored_key, profile);
                } else {
                    self.config_store
                        .delete_dialect_profile(&stored_key)
                        .await?;
                }
                continue;
            }

            let Some(key_fingerprint) = key_fingerprints
                .iter()
                .next()
                .filter(|_| key_fingerprints.len() == 1)
            else {
                self.config_store
                    .delete_dialect_profile(&stored_key)
                    .await?;
                continue;
            };
            let matching_exposed = exposed_models.iter().find(|exposed| {
                let legacy = Self::legacy_route_configuration_fingerprint_material_with_snapshot(
                    &fingerprint_snapshot,
                    upstream,
                    exposed,
                    &stored_key.runtime_model_slug,
                    protocol,
                );
                if legacy
                    .as_ref()
                    .is_ok_and(|fingerprint| fingerprint == &profile.configuration_fingerprint)
                {
                    return true;
                }
                Self::route_configuration_fingerprint_with_snapshot(
                    &fingerprint_snapshot,
                    upstream,
                    key_fingerprint,
                    exposed,
                    &stored_key.runtime_model_slug,
                    protocol,
                )
                .is_ok_and(|fingerprint| fingerprint == profile.configuration_fingerprint)
            });
            let Some(exposed_model) = matching_exposed else {
                self.config_store
                    .delete_dialect_profile(&stored_key)
                    .await?;
                continue;
            };
            let rebound_key = DialectProfileKey::for_key(
                upstream.id.clone(),
                key_fingerprint.clone(),
                stored_key.runtime_model_slug.clone(),
                stored_key.protocol,
            );
            if original_keys.contains(&rebound_key) || reconciled.contains_key(&rebound_key) {
                self.config_store
                    .delete_dialect_profile(&stored_key)
                    .await?;
                continue;
            }
            profile.key = rebound_key.clone();
            profile.configuration_fingerprint =
                Self::route_configuration_fingerprint_with_snapshot(
                    &fingerprint_snapshot,
                    upstream,
                    key_fingerprint,
                    exposed_model,
                    &stored_key.runtime_model_slug,
                    protocol,
                )?;
            self.config_store.upsert_dialect_profile(&profile).await?;
            self.config_store
                .delete_dialect_profile(&stored_key)
                .await?;
            reconciled.insert(rebound_key, profile);
        }
        self.capability_snapshot
            .store(Arc::new(CapabilityRuntimeSnapshot {
                configuration: compiled,
                profiles: reconciled,
            }));
        tracing::info!(
            capability_policy_bootstrapped = bootstrapped,
            capability_policy_revision = capability_state.configuration.revision,
            capability_policy_count = capability_state.configuration.policies.len(),
            "initialized capability policy"
        );
        Ok(())
    }

    fn capability_probe_jobs_for_upstream(
        &self,
        upstream: &UpstreamConfig,
    ) -> BTreeMap<DialectProfileKey, BTreeSet<String>> {
        if !upstream.active {
            return BTreeMap::new();
        }
        let mut jobs = BTreeMap::<DialectProfileKey, BTreeSet<String>>::new();
        for exposed_model in upstream.effective_downstream_models() {
            let Some(runtime_model_slug) = upstream.resolved_model_name(&exposed_model) else {
                continue;
            };
            for api_key in upstream.keys_for_model(&runtime_model_slug) {
                let key_fingerprint = upstream_key_fingerprint(&upstream.id, &api_key);
                for protocol in upstream.supported_protocols() {
                    jobs.entry(DialectProfileKey::for_key(
                        upstream.id.clone(),
                        key_fingerprint.clone(),
                        runtime_model_slug.clone(),
                        protocol.into(),
                    ))
                    .or_default()
                    .insert(exposed_model.clone());
                }
            }
        }
        jobs
    }

    async fn submit_capability_probe_jobs(
        &self,
        jobs: BTreeMap<DialectProfileKey, BTreeSet<String>>,
        reason: ProbeReason,
    ) -> io::Result<()> {
        let runtime_settings = self.runtime_settings();
        if jobs.is_empty()
            || (reason.is_automatic() && !runtime_settings.automatic_capability_probes_enabled)
        {
            return Ok(());
        }
        let routing = self.routing_snapshot().await;
        let capability_snapshot = self.capability_snapshot();
        let mut prepared_jobs = Vec::new();
        for (key, exposed_model_slugs) in jobs {
            let Some(exposed_model_slug) = exposed_model_slugs.iter().next() else {
                continue;
            };
            let Some(upstream) = routing
                .upstreams
                .iter()
                .find(|upstream| upstream.id == key.upstream_id)
            else {
                continue;
            };
            let protocol = match key.protocol {
                WireProtocol::ChatCompletions => UpstreamProtocol::ChatCompletions,
                WireProtocol::Responses => UpstreamProtocol::Responses,
                WireProtocol::Messages => continue,
            };
            let Some(mut job) = Self::build_capability_probe_job_for_key_with_snapshot(
                &capability_snapshot,
                upstream,
                &key.key_fingerprint,
                exposed_model_slug,
                &key.runtime_model_slug,
                protocol,
                reason,
                ProbeMode::Full,
            )?
            else {
                continue;
            };
            job.exposed_model_slugs = exposed_model_slugs;
            prepared_jobs.push(job);
        }
        if prepared_jobs.is_empty() {
            return Ok(());
        }
        let Some(sender) = ({
            self.capability_probe_sender
                .lock()
                .expect("probe sender lock poisoned")
                .clone()
        }) else {
            tracing::warn!(
                jobs = prepared_jobs.len(),
                "capability probe worker is not configured; reconcile will retry"
            );
            return Ok(());
        };
        let batch = ProbeJobBatch::new(prepared_jobs);
        match sender.try_send(batch) {
            Ok(()) => {}
            Err(TrySendError::Full(batch)) => {
                tracing::warn!(
                    jobs = batch.jobs().len(),
                    "capability probe queue is full; reconcile will retry"
                );
            }
            Err(TrySendError::Closed(batch)) => {
                tracing::warn!(
                    jobs = batch.jobs().len(),
                    "capability probe queue is closed; reconcile will retry"
                );
            }
        }
        Ok(())
    }

    fn stale_capability_probe_jobs_for_upstreams<'a>(
        &self,
        upstreams: impl IntoIterator<Item = &'a UpstreamConfig>,
        now: u64,
    ) -> BTreeMap<DialectProfileKey, BTreeSet<String>> {
        let snapshot = self.capability_snapshot();
        let refresh_interval_seconds = snapshot
            .configuration
            .source()
            .probe
            .refresh_interval_seconds;
        let mut jobs = BTreeMap::<DialectProfileKey, BTreeSet<String>>::new();
        for upstream in upstreams.into_iter().filter(|upstream| upstream.active) {
            for exposed_model in upstream.effective_downstream_models() {
                let Some(runtime_model_slug) = upstream.resolved_model_name(&exposed_model) else {
                    continue;
                };
                for api_key in upstream.keys_for_model(&runtime_model_slug) {
                    let key_fingerprint = upstream_key_fingerprint(&upstream.id, &api_key);
                    for protocol in upstream.supported_protocols() {
                        let key = DialectProfileKey::for_key(
                            upstream.id.clone(),
                            key_fingerprint.clone(),
                            runtime_model_slug.clone(),
                            protocol.into(),
                        );
                        let fingerprint = match Self::route_configuration_fingerprint_with_snapshot(
                            &snapshot,
                            upstream,
                            &key_fingerprint,
                            &exposed_model,
                            &runtime_model_slug,
                            protocol,
                        ) {
                            Ok(fingerprint) => fingerprint,
                            Err(error) => {
                                tracing::warn!(
                                    upstream_id = %upstream.id,
                                    exposed_model = %exposed_model,
                                    runtime_model = %runtime_model_slug,
                                    protocol = ?protocol,
                                    error = %error,
                                    "skipping capability probe for route with invalid fingerprint"
                                );
                                continue;
                            }
                        };
                        let current = snapshot.profiles.get(&key).is_some_and(|profile| {
                            capability_profile_is_current_or_retry_pending(
                                profile,
                                &fingerprint,
                                now,
                                refresh_interval_seconds,
                            )
                        });
                        if current {
                            continue;
                        }
                        jobs.entry(key).or_default().insert(exposed_model.clone());
                    }
                }
            }
        }
        jobs
    }

    pub async fn queue_capability_probes_for_downstream_model(
        &self,
        downstream_id: &str,
        model: &str,
    ) -> usize {
        let routing = self.routing_snapshot().await;
        let Some(downstream) = routing
            .downstreams
            .iter()
            .find(|downstream| downstream.id == downstream_id && downstream.active)
        else {
            return 0;
        };

        let mut queued = 0usize;
        for upstream in routing.upstreams.iter().filter(|upstream| upstream.active) {
            if !(downstream.model_allowlist.is_empty()
                || portal_model_is_allowed(&downstream.model_allowlist, model))
            {
                continue;
            }
            let Some(runtime_model_slug) = upstream.resolved_model_name(model) else {
                continue;
            };
            for api_key in upstream.keys_for_model(&runtime_model_slug) {
                let key_fingerprint = upstream_key_fingerprint(&upstream.id, &api_key);
                for protocol in upstream.supported_protocols() {
                    if let Ok(Some(job)) = self
                        .build_capability_probe_job(
                            &upstream.id,
                            &key_fingerprint,
                            model,
                            &runtime_model_slug,
                            protocol,
                            ProbeReason::Manual,
                        )
                        .await
                    {
                        queued += usize::from(self.queue_capability_probe(job));
                    }
                }
            }
        }
        queued
    }

    pub fn route_configuration_fingerprint(
        &self,
        upstream: &UpstreamConfig,
        key_fingerprint: &str,
        exposed_model_slug: &str,
        runtime_model_slug: &str,
        protocol: UpstreamProtocol,
    ) -> io::Result<String> {
        let snapshot = self.capability_snapshot();
        Self::route_configuration_fingerprint_with_snapshot(
            &snapshot,
            upstream,
            key_fingerprint,
            exposed_model_slug,
            runtime_model_slug,
            protocol,
        )
    }

    pub fn legacy_route_configuration_fingerprint(
        &self,
        upstream: &UpstreamConfig,
        exposed_model_slug: &str,
        runtime_model_slug: &str,
        protocol: UpstreamProtocol,
    ) -> io::Result<String> {
        let snapshot = self.capability_snapshot();
        Self::legacy_route_configuration_fingerprint_material_with_snapshot(
            &snapshot,
            upstream,
            exposed_model_slug,
            runtime_model_slug,
            protocol,
        )
    }

    pub fn route_configuration_fingerprint_with_snapshot(
        snapshot: &CapabilityRuntimeSnapshot,
        upstream: &UpstreamConfig,
        key_fingerprint: &str,
        exposed_model_slug: &str,
        runtime_model_slug: &str,
        protocol: UpstreamProtocol,
    ) -> io::Result<String> {
        let legacy = Self::legacy_route_configuration_fingerprint_material_with_snapshot(
            snapshot,
            upstream,
            exposed_model_slug,
            runtime_model_slug,
            protocol,
        )?;
        if key_fingerprint.trim().is_empty() {
            return Ok(legacy);
        }
        let mut hasher = sha2::Sha256::new();
        hasher.update(legacy.as_bytes());
        hasher.update(b"\0");
        hasher.update(key_fingerprint.as_bytes());
        Ok(format!("{:x}", hasher.finalize()))
    }

    pub fn legacy_route_configuration_fingerprint_with_snapshot(
        snapshot: &CapabilityRuntimeSnapshot,
        upstream: &UpstreamConfig,
        exposed_model_slug: &str,
        runtime_model_slug: &str,
        protocol: UpstreamProtocol,
    ) -> io::Result<String> {
        Self::legacy_route_configuration_fingerprint_material_with_snapshot(
            snapshot,
            upstream,
            exposed_model_slug,
            runtime_model_slug,
            protocol,
        )
    }

    fn legacy_route_configuration_fingerprint_material_with_snapshot(
        snapshot: &CapabilityRuntimeSnapshot,
        upstream: &UpstreamConfig,
        exposed_model_slug: &str,
        runtime_model_slug: &str,
        protocol: UpstreamProtocol,
    ) -> io::Result<String> {
        let normalized_base_url = normalize_route_base_url(&upstream.base_url)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let mut route = crate::capabilities::RouteIdentity {
            upstream_id: upstream.id.clone(),
            key_fingerprint: String::new(),
            exposed_model_slug: exposed_model_slug.to_owned(),
            runtime_model_slug: runtime_model_slug.to_owned(),
            protocol: WireProtocol::from(protocol),
            tags: BTreeSet::new(),
        };
        snapshot.configuration.apply_route_tags(&mut route);
        let route_overrides = snapshot.configuration.route_overrides_for(&route);
        let route_override_digest = format!(
            "{:x}",
            sha2::Sha256::digest(
                serde_json::to_vec(&route_overrides)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
            )
        );
        Ok(route_fingerprint(&RouteFingerprintInput {
            normalized_base_url,
            enabled_protocols: upstream
                .supported_protocols()
                .into_iter()
                .map(WireProtocol::from)
                .collect(),
            runtime_model_slug: runtime_model_slug.to_owned(),
            route_override_digest,
            probe_schema_version: crate::capabilities::DIALECT_PROBE_SCHEMA_VERSION,
        }))
    }

    pub async fn qualify_active_upstreams(
        &self,
        upstream_ids: &[String],
    ) -> io::Result<Vec<UpstreamQualificationDecision>> {
        #[derive(Debug)]
        struct ProbeRecord {
            api_key: String,
            model: String,
            protocol: UpstreamProtocol,
            categories: Vec<ModelQualificationCategory>,
            latency_ms: u64,
        }

        let runtime_settings = self.runtime_settings();
        let selected = upstream_ids
            .iter()
            .map(|id| id.trim())
            .filter(|id| !id.is_empty())
            .collect::<HashSet<_>>();
        let routing = self.routing_snapshot().await;
        let capability_snapshot = self.capability_snapshot();
        let refresh_interval_seconds = capability_snapshot
            .configuration
            .source()
            .probe
            .refresh_interval_seconds;
        let timeout_seconds = runtime_settings.admin_upstream_timeout_seconds.max(1);
        let attempted_at = unix_seconds();
        let mut decisions = Vec::new();

        for upstream in Arc::unwrap_or_clone(routing.upstreams)
            .into_iter()
            .filter(|upstream| {
                upstream.active && (selected.is_empty() || selected.contains(upstream.id.as_str()))
            })
        {
            let keys = upstream.available_keys();
            let client = self.client_for_url(&upstream.base_url);
            let discovery_results = fetch_models_from_upstream_keys_concurrently(
                &client,
                &upstream.base_url,
                &keys,
                timeout_seconds,
            )
            .await;
            let discovered_by_key = discovery_results
                .into_iter()
                .filter_map(|result| {
                    keys.get(result.key_index).map(|api_key| {
                        (
                            api_key.clone(),
                            result.models.into_iter().collect::<BTreeSet<_>>(),
                        )
                    })
                })
                .collect::<HashMap<_, _>>();
            let aggregate_models = upstream.route_models().into_iter().collect::<BTreeSet<_>>();
            let uses_per_key_maps = !upstream.api_key_models.is_empty();
            let mut previous_by_key = BTreeMap::<String, BTreeSet<String>>::new();
            let mut candidates_by_key = BTreeMap::<String, BTreeSet<String>>::new();

            for api_key in &keys {
                let previous = if uses_per_key_maps {
                    upstream
                        .api_key_models
                        .iter()
                        .filter(|mapping| mapping.api_key.trim() == api_key)
                        .flat_map(|mapping| mapping.supported_models.iter())
                        .map(|model| model.trim())
                        .filter(|model| !model.is_empty())
                        .map(str::to_string)
                        .collect::<BTreeSet<_>>()
                } else {
                    aggregate_models.clone()
                };
                let mut candidates = previous.clone();
                if let Some(discovered) = discovered_by_key.get(api_key) {
                    candidates.extend(discovered.iter().cloned());
                }
                previous_by_key.insert(api_key.clone(), previous);
                candidates_by_key.insert(api_key.clone(), candidates);
            }

            let protocols = upstream.supported_protocols();
            let jobs = candidates_by_key
                .iter()
                .flat_map(|(api_key, models)| {
                    protocols.iter().flat_map(move |protocol| {
                        models
                            .iter()
                            .map(move |model| (api_key.clone(), model.clone(), *protocol))
                    })
                })
                .collect::<Vec<_>>();
            let base_url = upstream.base_url.clone();
            let records = stream::iter(jobs.into_iter().map(|(api_key, model, protocol)| {
                let client = client.clone();
                let base_url = base_url.clone();
                async move {
                    let first = qualify_model_on_upstream(
                        &client,
                        &base_url,
                        &api_key,
                        &model,
                        protocol,
                        timeout_seconds,
                    )
                    .await;
                    let mut latency_ms = first.latency_ms;
                    let mut categories = vec![first.category];
                    if first.category.requires_confirmation() {
                        let confirmation = qualify_model_on_upstream(
                            &client,
                            &base_url,
                            &api_key,
                            &model,
                            protocol,
                            timeout_seconds,
                        )
                        .await;
                        latency_ms = latency_ms.saturating_add(confirmation.latency_ms);
                        categories.push(confirmation.category);
                    }
                    ProbeRecord {
                        api_key,
                        model,
                        protocol,
                        categories,
                        latency_ms,
                    }
                }
            }))
            .buffer_unordered(4)
            .collect::<Vec<_>>()
            .await;

            let mut evidence = Vec::with_capacity(records.len());
            let mut levels_by_key_model =
                HashMap::<(String, String), Vec<ModelQualificationLevel>>::new();
            for record in records {
                let passed = record
                    .categories
                    .contains(&ModelQualificationCategory::Passed);
                let key_fingerprint = upstream_key_fingerprint(&upstream.id, &record.api_key);
                let route_id = anonymous_route_id(
                    &upstream.id,
                    &key_fingerprint,
                    &record.model,
                    WireProtocol::from(record.protocol),
                );
                let level = if passed {
                    let profile_key = DialectProfileKey::for_key(
                        upstream.id.clone(),
                        key_fingerprint.clone(),
                        record.model.clone(),
                        WireProtocol::from(record.protocol),
                    );
                    let profile = Self::route_configuration_fingerprint_with_snapshot(
                        &capability_snapshot,
                        &upstream,
                        &key_fingerprint,
                        &record.model,
                        &record.model,
                        record.protocol,
                    )
                    .ok()
                    .and_then(|fingerprint| {
                        capability_snapshot
                            .profiles
                            .get(&profile_key)
                            .filter(|profile| {
                                profile_is_current(
                                    profile,
                                    &fingerprint,
                                    attempted_at,
                                    refresh_interval_seconds,
                                )
                            })
                    });
                    classify_qualification_level(ModelQualificationCategory::Passed, profile)
                } else {
                    confirmed_level(&record.categories)
                };
                let category = if passed {
                    ModelQualificationCategory::Passed
                } else if let Some(operational) = record
                    .categories
                    .iter()
                    .copied()
                    .find(|category| category.is_operational())
                {
                    operational
                } else {
                    record.categories[0]
                };
                evidence.push(ModelQualificationEvidence {
                    upstream_id: upstream.id.clone(),
                    route_id,
                    model: record.model.clone(),
                    protocol: record.protocol,
                    level,
                    category,
                    latency_ms: record.latency_ms,
                    attempted_at,
                });
                levels_by_key_model
                    .entry((record.api_key, record.model))
                    .or_default()
                    .push(level);
            }

            let mut key_decisions = Vec::with_capacity(keys.len());
            for api_key in keys {
                let observations = candidates_by_key
                    .remove(&api_key)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|model| {
                        let levels = levels_by_key_model
                            .remove(&(api_key.clone(), model.clone()))
                            .unwrap_or_default();
                        let level = if levels.contains(&ModelQualificationLevel::Full) {
                            ModelQualificationLevel::Full
                        } else if levels.contains(&ModelQualificationLevel::Adapted) {
                            ModelQualificationLevel::Adapted
                        } else if levels.contains(&ModelQualificationLevel::OperationalFailure) {
                            ModelQualificationLevel::OperationalFailure
                        } else {
                            ModelQualificationLevel::Unusable
                        };
                        QualificationObservation { model, level }
                    })
                    .collect();
                let mut decision = build_key_qualification_decision(
                    previous_by_key.remove(&api_key).unwrap_or_default(),
                    observations,
                );
                decision.api_key = api_key;
                key_decisions.push(decision);
            }

            evidence.sort_by(|left, right| {
                left.route_id
                    .cmp(&right.route_id)
                    .then_with(|| left.model.cmp(&right.model))
                    .then_with(|| {
                        let rank = |protocol| match protocol {
                            UpstreamProtocol::ChatCompletions => 0,
                            UpstreamProtocol::Responses => 1,
                        };
                        rank(left.protocol).cmp(&rank(right.protocol))
                    })
            });
            decisions.push(UpstreamQualificationDecision {
                upstream_id: upstream.id,
                keys: key_decisions,
                evidence,
            });
        }

        Ok(decisions)
    }

    pub async fn apply_model_qualification(
        &self,
        decisions: Vec<UpstreamQualificationDecision>,
        downstream_id: &str,
    ) -> io::Result<ModelQualificationApplySummary> {
        if decisions.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "qualification produced no upstream decisions",
            ));
        }
        let downstream_id = downstream_id.trim().to_string();
        if downstream_id.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "downstream_id is required",
            ));
        }

        let result = self
            .mutate_persisted_state_io(move |state| {
                let mut updated = HashSet::new();
                for decision in &decisions {
                    if !updated.insert(decision.upstream_id.clone()) {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "duplicate upstream qualification decision",
                        ));
                    }
                    let upstream = Arc::make_mut(&mut state.upstreams)
                        .iter_mut()
                        .find(|value| value.id == decision.upstream_id)
                        .ok_or_else(|| {
                            io::Error::new(io::ErrorKind::NotFound, "upstream disappeared")
                        })?;
                    let configured_keys = upstream
                        .available_keys()
                        .into_iter()
                        .collect::<HashSet<_>>();
                    if decision
                        .keys
                        .iter()
                        .any(|key| !configured_keys.contains(key.api_key.trim()))
                    {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "qualification referenced an unknown upstream key",
                        ));
                    }

                    upstream.api_key_models = decision
                        .keys
                        .iter()
                        .map(|key| ApiKeyModelConfig {
                            api_key: key.api_key.clone(),
                            supported_models: key.retained.iter().cloned().collect(),
                        })
                        .collect();
                    let retained = decision
                        .keys
                        .iter()
                        .flat_map(|key| key.retained.iter().cloned())
                        .collect::<BTreeSet<_>>();
                    upstream
                        .premium_models
                        .retain(|model| retained.contains(model));
                    upstream.supported_models = retained
                        .into_iter()
                        .filter(|model| !upstream.premium_models.contains(model))
                        .collect();
                    upstream.normalize_for_storage();
                    upstream
                        .validate_configuration()
                        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
                }

                let exposed = state
                    .upstreams
                    .iter()
                    .filter(|upstream| upstream.active)
                    .flat_map(UpstreamConfig::route_models)
                    .collect::<BTreeSet<_>>();
                if exposed.is_empty() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "qualification would remove the final routable model",
                    ));
                }
                let downstream = Arc::make_mut(&mut state.downstreams)
                    .iter_mut()
                    .find(|value| value.id == downstream_id)
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::NotFound, "downstream not found")
                    })?;
                downstream.model_allowlist = exposed.iter().cloned().collect();

                Ok(ModelQualificationApplySummary {
                    upstreams_updated: updated.len(),
                    retained_models: exposed.len(),
                })
            })
            .await?;
        let current_upstreams = self.routing_snapshot().await.upstreams;
        self.reconcile_route_health(&current_upstreams)
            .await
            .map_err(io::Error::other)?;
        Ok(result)
    }

    async fn mutate_persisted_state<T, E, F, M>(&self, mutator: F, map_io: M) -> Result<T, E>
    where
        F: FnOnce(&mut PersistedState) -> Result<T, E>,
        M: Fn(io::Error) -> E,
    {
        let _persist_guard = self.config_persist_lock.lock().await;
        let mut state = self.inner.lock().await;
        let mut candidate_state = state.clone();
        let result = mutator(&mut candidate_state)?;
        if !downstream_plaintext_pairs_unchanged(&state.downstreams, &candidate_state.downstreams) {
            validate_downstream_plaintext_pairs(&mut candidate_state);
        }

        self.config_store
            .persist_config(&candidate_state)
            .await
            .map_err(map_io)?;

        state.upstreams = candidate_state.upstreams;
        state.downstreams = candidate_state.downstreams;
        state.announcement = candidate_state.announcement;
        state.global_context_profiles = candidate_state.global_context_profiles;
        state.runtime_settings = candidate_state.runtime_settings;

        Ok(result)
    }

    async fn mutate_persisted_state_io<T, F>(&self, mutator: F) -> io::Result<T>
    where
        F: FnOnce(&mut PersistedState) -> io::Result<T>,
    {
        self.mutate_persisted_state(mutator, |error| error).await
    }

    async fn persist_state(&self, state: &PersistedState) -> io::Result<()> {
        let _persist_guard = self.config_persist_lock.lock().await;
        self.config_store.persist_config(state).await
    }

    async fn enforce_usage_log_archive_limit(&self) -> io::Result<()> {
        let limit = self.config.usage_log_archive_max_files.max(1);
        let archive_paths = usage_log_archive_paths(&self.store_path).await?;
        if archive_paths.len() <= limit {
            return Ok(());
        }

        let remove_count = archive_paths.len() - limit;
        let mut removed_ids = HashSet::new();

        for path in archive_paths.into_iter().take(remove_count) {
            let logs = load_usage_log_archive(&path).await?;
            removed_ids.extend(logs.into_iter().map(|log| log.id));
            match fs::remove_file(&path).await {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }

        if !removed_ids.is_empty() {
            let mut archived = self.archived_usage_logs.lock().await;
            archived.retain(|log| !removed_ids.contains(&log.id));
        }

        Ok(())
    }
}
async fn load_archived_usage_logs(store_path: &Path) -> io::Result<Vec<UsageLog>> {
    let archive_paths = usage_log_archive_paths(store_path).await?;

    let mut usage_logs = Vec::new();
    for path in archive_paths {
        let mut archived = load_usage_log_archive(&path).await?;
        usage_logs.append(&mut archived);
    }

    Ok(usage_logs)
}

async fn usage_log_archive_paths(store_path: &Path) -> io::Result<Vec<PathBuf>> {
    let Some(parent) = store_path.parent() else {
        return Ok(Vec::new());
    };
    let Some(base_name) = store_path.file_name().and_then(|value| value.to_str()) else {
        return Ok(Vec::new());
    };

    let archive_prefix = format!("{base_name}.usage.");
    let mut dir = fs::read_dir(parent).await?;
    let mut archive_paths = Vec::new();

    while let Some(entry) = dir.next_entry().await? {
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if file_name.starts_with(&archive_prefix) && file_name.ends_with(".json") {
            let sort_key = load_usage_log_archive(&path)
                .await
                .ok()
                .and_then(|logs| logs.first().map(|log| log.created_at))
                .unwrap_or(0);
            archive_paths.push((sort_key, file_name.to_string(), path));
        }
    }

    archive_paths.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
    Ok(archive_paths.into_iter().map(|(_, _, path)| path).collect())
}

async fn load_usage_log_archive(path: &Path) -> io::Result<Vec<UsageLog>> {
    let bytes = fs::read(path).await?;
    Ok(serde_json::from_slice(&bytes).unwrap_or_default())
}

fn truncate_active_request_user_agent(user_agent: String) -> String {
    if user_agent.len() <= ACTIVE_REQUEST_USER_AGENT_MAX_BYTES {
        return user_agent;
    }

    let mut end = ACTIVE_REQUEST_USER_AGENT_MAX_BYTES;
    while !user_agent.is_char_boundary(end) {
        end -= 1;
    }
    user_agent[..end].to_string()
}

/// C1.1: The kind of an upstream account lease.  Distinguishes long-running
/// streaming requests from short unary calls so the stale reclamation (C2.3)
/// and the release path can treat them differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamLeaseKind {
    Streaming,
    Unary,
}

/// E3: per-account ring of observed lease hold durations (release − reserve),
/// sized to a fixed window so the p50/p95 estimate tracks recent service time
/// instead of drifting toward a stale epoch.  Driven by the C1/C2 lease
/// release path: every successful local release appends its hold duration.
const LEASE_HOLD_SAMPLE_SIZE: usize = 32;

/// E4.2: the C3 local-slot queue budget is `p95_hold × this factor`, so the
/// wait covers ~all observed service times (p95) with a small margin, instead
/// of the flat pre-E4 static budget.
const ADAPTIVE_QUEUE_BUDGET_FACTOR: f64 = 1.5;
/// E4.2: hard ceiling on the adaptive queue budget.  The evidence-based
/// budget never goes to minutes-long silent waits even for pathological p95 —
/// a duration that long is better served by an honest fast-fail (E4 §3.4:
/// do not turn a doomed wait into minutes of silence).
const ADAPTIVE_QUEUE_BUDGET_CEILING_MS: u64 = 60_000;

/// C1.1: A single upstream account lease record.  Replaces the bare
/// `expires_at` `Instant` that used to sit in `UpstreamRuntimeState`:
/// `last_renewed_at` lets the stale sweep tell a live-but-silent lease apart
/// from a leaked one without waiting out the full TTL, and `kind` records the
/// request shape (C2.3 logs it on reclamation).
#[derive(Debug, Clone)]
pub struct UpstreamLeaseRecord {
    #[allow(dead_code)] // `kind` is read by the C2.3 stale-reclamation logging.

    /// Absolute expiry (now + TTL).  Lazy sweeps reclaim leases whose expiry
    /// has passed.
    expires_at: tokio::time::Instant,
    /// Last heartbeat / renewal time (C2.1).  The stale sweep reclaims leases
    /// that have not been renewed for longer than
    /// `upstream_lease_stale_after_ms`.
    last_renewed_at: tokio::time::Instant,
    /// E3: when this lease was reserved.  Immutable (heartbeats only touch
    /// `last_renewed_at` / `expires_at`), so `release − reserved_at` is the
    /// true time the upstream slot was held — the quantity that decides when
    /// a slot actually frees.  The C2 heartbeat keeps `expires_at` pinned at
    /// ~full TTL, which is exactly why TTL-based Retry-After estimates are
    /// meaningless for live requests.
    reserved_at: tokio::time::Instant,
    kind: UpstreamLeaseKind,
}

impl UpstreamLeaseRecord {
    fn new(now: tokio::time::Instant, ttl: Duration, kind: UpstreamLeaseKind) -> Self {
        Self {
            expires_at: now + ttl,
            last_renewed_at: now,
            reserved_at: now,
            kind,
        }
    }
}

/// C1.1: The upstream account lease table.  Split out of `UpstreamRuntimeState`
/// so it can sit behind a plain `std::sync::Mutex` and be mutated synchronously
/// from a `Drop` without a Tokio runtime (C1.2) and without `try_lock` races
/// (C1.4).  Quota events, cooldowns and feedback stay in `UpstreamRuntimeState`
/// (they are only ever touched from async contexts); only leases and the
/// reclamation counters live here.
///
/// Lock discipline: every critical section on this table is purely synchronous
/// and never spans an `await`.  Callers that also need `UpstreamRuntimeState`
/// must acquire the Tokio runtime-state lock first and this lock second, never
/// the reverse.
#[derive(Debug, Default)]
pub struct UpstreamLeaseTable {
    leases: HashMap<AccountConcurrencyKey, HashMap<String, UpstreamLeaseRecord>>,
    /// E3: per-account ring of observed lease hold durations (release −
    /// reserve), bounded to `LEASE_HOLD_SAMPLE_SIZE`.  Drives the p50/p95
    /// Retry-After estimate for local-concurrency rejections — measuring the
    /// *time slots are actually held* (service time) rather than the lease TTL.
    hold_samples: HashMap<AccountConcurrencyKey, VecDeque<Duration>>,
    /// Per-upstream cumulative count of expired local leases reclaimed by the
    /// lazy sweeps (P7 observability): zero means no leak has ever actually
    /// happened.  Keyed by upstream_id.
    leaked_reclaimed_total: HashMap<String, u64>,
    /// C2.3: per-upstream cumulative count of *stale* leases (not heartbeated
    /// within `upstream_lease_stale_after_ms`) reclaimed by the lazy sweeps.
    /// Kept separate from `leaked_reclaimed_total` so an operator can tell the
    /// two reclaim modes apart.
    stale_reclaimed_total: HashMap<String, u64>,
    /// E5.3: per-upstream cumulative count of local-gate rejections
    /// (`LocalConcurrency` — the pre-dispatch slot gate said "full").  With
    /// C3/E4 this is now the *snowplow safety net* firing (or the E4.2
    /// skip-queue fast-fail), so a non-zero value is the tell that the
    /// upstream's real concurrency was saturated at the gateway's own gate.
    capacity_reject_total: HashMap<String, u64>,
}

impl UpstreamLeaseTable {
    fn account_lease_count(&self, account: &AccountConcurrencyKey) -> usize {
        self.leases.get(account).map_or(0, HashMap::len)
    }

    /// Per-upstream total across all keys.  Snapshot `in_flight` must stay
    /// per-upstream (the pre-C1 count lived inside each upstream's own
    /// `UpstreamRuntimeState`); returning the global total here would leak one
    /// upstream's leases into every other upstream's snapshot.  Also the C5.1
    /// snapshot surface.
    fn account_lease_count_for_upstream(&self, upstream_id: &str) -> usize {
        self.leases
            .iter()
            .filter(|(account, _)| account.upstream_id == upstream_id)
            .map(|(_, leases)| leases.len())
            .sum()
    }

    /// C4.2: leases for `account` whose last heartbeat is older than
    /// `stale_after`.  The stale sweep (C2.3) uses the same predicate, so this
    /// number is exactly what that sweep would reclaim right now.
    fn account_stale_lease_count(
        &self,
        account: &AccountConcurrencyKey,
        stale_after: Duration,
        now: tokio::time::Instant,
    ) -> usize {
        self.leases
            .get(account)
            .map(|leases| {
                leases
                    .values()
                    .filter(|record| {
                        now.saturating_duration_since(record.last_renewed_at) > stale_after
                    })
                    .count()
            })
            .unwrap_or(0)
    }

    /// C5.1: leases currently held for `upstream_id` (all keys whose heartbeat
    /// is older than `stale_after`).  Same predicate as the stale sweep; fed
    /// into `UpstreamRuntimeSnapshot.stale_lease_count`.
    fn upstream_stale_lease_count(
        &self,
        upstream_id: &str,
        stale_after: Duration,
        now: tokio::time::Instant,
    ) -> usize {
        self.leases
            .iter()
            .filter(|(account, _)| account.upstream_id == upstream_id)
            .map(|(_, leases)| {
                leases
                    .values()
                    .filter(|record| {
                        now.saturating_duration_since(record.last_renewed_at) > stale_after
                    })
                    .count()
            })
            .sum()
    }

    /// C5.1: age in seconds of the oldest currently-held lease for
    /// `upstream_id`, i.e. the maximum `now - last_renewed_at` across all its
    /// accounts' leases.  0 when the upstream holds no leases.
    fn upstream_oldest_lease_age_seconds(
        &self,
        upstream_id: &str,
        now: tokio::time::Instant,
    ) -> u64 {
        self.leases
            .iter()
            .filter(|(account, _)| account.upstream_id == upstream_id)
            .flat_map(|(_, leases)| leases.values())
            .map(|record| {
                now.saturating_duration_since(record.last_renewed_at)
                    .as_secs()
            })
            .max()
            .unwrap_or(0)
    }

    /// C5.2: remove every lease belonging to `upstream_id`, optionally
    /// restricted to a single `key_fingerprint`.  Returns how many leases were
    /// removed.  Does not touch reclamation counters: this is an operator
    /// reset, not a reclamation event.
    fn remove_leases_for_upstream(
        &mut self,
        upstream_id: &str,
        key_fingerprint: Option<&str>,
    ) -> usize {
        let mut removed = 0usize;
        self.leases.retain(|account, leases| {
            if account.upstream_id != upstream_id {
                return true;
            }
            if key_fingerprint.is_some_and(|fp| fp != account.key_fingerprint) {
                return true;
            }
            removed += leases.len();
            false
        });
        removed
    }

    fn insert(
        &mut self,
        account: AccountConcurrencyKey,
        lease_id: String,
        record: UpstreamLeaseRecord,
    ) {
        self.leases
            .entry(account)
            .or_default()
            .insert(lease_id, record);
    }

    /// E3: remove a lease and, if it was actually held, record its hold
    /// duration (now − `reserved_at`) into the account's sample ring.  The
    /// hold duration is what decides when the slot frees — the C2 heartbeat
    /// keeps the TTL pinned near full, so remaining-TTL bookkeeping cannot
    /// answer "how long until a slot opens".  Returns whether a lease was
    /// removed.  Only real releases end up in the ring: leases reclaimed by
    /// the lazy sweeps (which never ran a release path) are not sampled.
    fn remove(
        &mut self,
        account: &AccountConcurrencyKey,
        lease_id: &str,
        now: tokio::time::Instant,
    ) -> bool {
        let Some(record) = self
            .leases
            .get_mut(account)
            .and_then(|leases| leases.remove(lease_id))
        else {
            return false;
        };
        let hold = now.saturating_duration_since(record.reserved_at);
        let samples = self.hold_samples.entry(account.clone()).or_default();
        if samples.len() >= LEASE_HOLD_SAMPLE_SIZE {
            samples.pop_front();
        }
        samples.push_back(hold);
        if self.leases.get(account).map_or(true, HashMap::is_empty) {
            self.leases.remove(account);
        }
        true
    }

    /// E3: p50 observed hold duration (release − reserve) for `account`, in
    /// whole seconds.  `None` when fewer than two samples exist — with a
    /// single sample there is no central tendency worth trusting.
    fn hold_p50_seconds(&self, account: &AccountConcurrencyKey) -> Option<u64> {
        let mut holds = self
            .hold_samples
            .get(account)
            .map(|samples| samples.iter().copied().collect::<Vec<_>>())?
            .into_iter()
            .collect::<Vec<_>>();
        if holds.len() < 2 {
            return None;
        }
        holds.sort_unstable();
        let mid = holds.len() / 2;
        Some(holds[mid].as_secs())
    }

    /// E3: p95 observed hold duration for `account`, in whole seconds.
    /// `None` when fewer than two samples exist (same discipline as
    /// [`Self::hold_p50_seconds`]).
    fn hold_p95_seconds(&self, account: &AccountConcurrencyKey) -> Option<u64> {
        let mut holds = self
            .hold_samples
            .get(account)
            .map(|samples| samples.iter().copied().collect::<Vec<_>>())?
            .into_iter()
            .collect::<Vec<_>>();
        if holds.len() < 2 {
            return None;
        }
        holds.sort_unstable();
        let idx = ((holds.len() as f64) * 0.95).ceil() as usize;
        Some(holds[idx.saturating_sub(1).min(holds.len() - 1)].as_secs())
    }

    /// E5.3: p50 observed hold duration for `upstream_id` across all its
    /// accounts, in milliseconds (aggregates the per-account `hold_samples`).
    /// `None` when fewer than two samples exist for the upstream overall.
    fn upstream_hold_p50_ms(&self, upstream_id: &str) -> Option<u64> {
        let mut holds = self
            .hold_samples
            .iter()
            .filter(|(account, _)| account.upstream_id == upstream_id)
            .flat_map(|(_, samples)| samples.iter().copied())
            .collect::<Vec<_>>();
        if holds.len() < 2 {
            return None;
        }
        holds.sort_unstable();
        let idx = ((holds.len() as f64) * 0.50).ceil() as usize;
        Some(
            holds[idx.saturating_sub(1).min(holds.len() - 1)]
                .as_millis()
                .min(u64::MAX as u128) as u64,
        )
    }

    /// E5.3: p95 observed hold duration for `upstream_id` across all its
    /// accounts, in milliseconds (see [`Self::upstream_hold_p50_ms`]).
    fn upstream_hold_p95_ms(&self, upstream_id: &str) -> Option<u64> {
        let mut holds = self
            .hold_samples
            .iter()
            .filter(|(account, _)| account.upstream_id == upstream_id)
            .flat_map(|(_, samples)| samples.iter().copied())
            .collect::<Vec<_>>();
        if holds.len() < 2 {
            return None;
        }
        holds.sort_unstable();
        let idx = ((holds.len() as f64) * 0.95).ceil() as usize;
        Some(
            holds[idx.saturating_sub(1).min(holds.len() - 1)]
                .as_millis()
                .min(u64::MAX as u128) as u64,
        )
    }

    /// E5.3: cumulative local-gate rejections for `upstream_id`.
    fn capacity_reject_total(&self, upstream_id: &str) -> u64 {
        self.capacity_reject_total
            .get(upstream_id)
            .copied()
            .unwrap_or(0)
    }

    /// E3: the longest *already-held* duration among `account`'s live leases
    /// (now − `reserved_at`).  This is the portion of the expected service
    /// time already consumed by the oldest occupant — the best available
    /// proxy for "how much of the p50 window is left before the first slot
    /// frees".  0 when no lease is held.
    fn oldest_held_seconds(
        &self,
        account: &AccountConcurrencyKey,
        now: tokio::time::Instant,
    ) -> u64 {
        self.leases
            .get(account)
            .map(|leases| {
                leases
                    .values()
                    .map(|record| now.saturating_duration_since(record.reserved_at).as_secs())
                    .max()
                    .unwrap_or(0)
            })
            .unwrap_or(0)
    }

    fn renew(
        &mut self,
        account: &AccountConcurrencyKey,
        lease_id: &str,
        now: tokio::time::Instant,
        ttl: Duration,
    ) {
        if let Some(record) = self
            .leases
            .get_mut(account)
            .and_then(|leases| leases.get_mut(lease_id))
        {
            record.expires_at = now + ttl;
            record.last_renewed_at = now;
        }
    }

    /// C2.1: flips a lease's kind to Streaming once the first chunk is seen.
    #[allow(dead_code)] // used by the C2.1 heartbeat patch in gateway.rs.
    fn mark_streaming(&mut self, account: &AccountConcurrencyKey, lease_id: &str) -> bool {
        match self
            .leases
            .get_mut(account)
            .and_then(|leases| leases.get_mut(lease_id))
        {
            Some(record) => {
                record.kind = UpstreamLeaseKind::Streaming;
                true
            }
            None => false,
        }
    }

    fn remove_upstream(&mut self, upstream_id: &str) {
        self.leases
            .retain(|account, _| account.upstream_id != upstream_id);
        self.leaked_reclaimed_total.remove(upstream_id);
        self.stale_reclaimed_total.remove(upstream_id);
    }
}

/// C1.1 + C2.3: Lazy reclamation of local upstream leases: the in-memory
/// counterpart of the Lua `ZREMRANGEBYSCORE` sweep at the top of
/// `upstream_reserve.lua`.  Reclaims leases whose absolute expiry has passed
/// (counted into `leaked_reclaimed_total`) and leases whose last heartbeat is
/// older than `upstream_lease_stale_after_ms` (counted separately into
/// `stale_reclaimed_total`, C2.3).  Returns how many leases were reclaimed on
/// this call.  Never runs inside an `await` (C1.1 lock discipline).
fn prune_expired_upstream_leases(
    table: &mut UpstreamLeaseTable,
    now: tokio::time::Instant,
    config: &AppConfig,
) -> u64 {
    let stale_after = Duration::from_millis(config.upstream_lease_stale_after_ms.max(1));
    let mut reclaimed = 0u64;
    for (account, leases) in table.leases.iter_mut() {
        // C5.3: reclamation is an event worth a warn — a lease that was never
        // released is either a leaked guard (TTL expiry) or a dead owner (stale
        // heartbeat).  Log each lease individually with the facts an operator
        // needs to hunt the leak (upstream, key, lease id, age, kind).
        let mut expired = Vec::new();
        let mut stale = Vec::new();
        leases.retain(|lease_id, record| {
            if record.expires_at <= now {
                expired.push((lease_id.clone(), record.clone()));
                return false;
            }
            if now.saturating_duration_since(record.last_renewed_at) > stale_after {
                stale.push((lease_id.clone(), record.clone()));
                return false;
            }
            true
        });
        for (lease_id, record) in expired {
            tracing::warn!(
                upstream_id = %account.upstream_id,
                key_fingerprint = %account.key_fingerprint,
                lease_id = %lease_id,
                age_ms = now.saturating_duration_since(record.last_renewed_at).as_millis() as u64,
                kind = ?record.kind,
                "local upstream lease reclaimed by TTL expiry (possibly leaked guard)"
            );
            *table
                .leaked_reclaimed_total
                .entry(account.upstream_id.clone())
                .or_default() += 1;
            reclaimed += 1;
        }
        for (lease_id, record) in stale {
            tracing::warn!(
                upstream_id = %account.upstream_id,
                key_fingerprint = %account.key_fingerprint,
                lease_id = %lease_id,
                age_ms = now.saturating_duration_since(record.last_renewed_at).as_millis() as u64,
                kind = ?record.kind,
                "local upstream lease reclaimed as stale: no heartbeat within upstream_lease_stale_after_ms"
            );
            *table
                .stale_reclaimed_total
                .entry(account.upstream_id.clone())
                .or_default() += 1;
            reclaimed += 1;
        }
    }
    table.leases.retain(|_, leases| !leases.is_empty());
    reclaimed
}

/// E3: Retry-After estimate for a local-concurrency rejection, measured on
/// the *right* object.  The pre-E3 code used the oldest active lease's
/// remaining TTL (`oldest_remaining_secs`) — but the C2 heartbeat renews
/// every live lease every ttl/3, so for any live request that value stays
/// pinned near the full TTL (300s) and, after the 30s cap, the gateway
/// advertised a constant ~30s regardless of how long a slot really takes to
/// free.  That constant exactly matched the observed "retried for 32.1s
/// across 6 rounds" pattern while the upstream was never even contacted.
///
/// E3 instead estimates from observed *hold* durations (release − reserve):
///   retry_after ≈ p50_hold − oldest_lease_already_held
/// i.e. "the median request takes p50_hold; the oldest occupant has already
/// consumed `oldest_held` of that window, so the first slot frees in roughly
/// the remainder".  Floored with the first probe-delay tier (fast-moving
/// queues must not advertise sub-second waits a retry loop cannot use).
/// With no samples yet there is no observation to lean on, so it falls back
/// to the static probe-delay floor rather than pretending to know the server.
fn estimate_local_concurrency_retry_after_seconds(
    config: &AppConfig,
    table: &UpstreamLeaseTable,
    account: &AccountConcurrencyKey,
    now: tokio::time::Instant,
) -> u64 {
    let probe_delay_secs = config
        .upstream_concurrency_probe_delays_ms
        .first()
        .copied()
        .map(|ms| (ms / 1_000).max(1))
        .unwrap_or(1);
    let Some(p50_hold) = table.hold_p50_seconds(account) else {
        return probe_delay_secs;
    };
    let oldest_held = table.oldest_held_seconds(account, now);
    p50_hold
        .saturating_sub(oldest_held)
        .max(probe_delay_secs)
        .max(1)
}

#[derive(Debug, Clone, Default)]
struct UpstreamRuntimeState {
    minute_events: VecDeque<QuotaEvent>,
    five_hour_events: VecDeque<QuotaEvent>,
    cooldown_until: u64,
    last_feedback_type: Option<String>,
    last_retry_after_seconds: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct UpstreamRuntimeSnapshot {
    pub in_flight: u32,
    pub minute_cost: f64,
    pub five_hour_cost: f64,
    pub cooldown_until: u64,
    /// Local backend only: expired leases reclaimed by lazy sweeps (P7).
    /// Always 0 on the Redis backend (its Lua sweeps self-heal natively).
    pub leaked_reclaimed_total: u64,
    /// C2.3: local backend only: leases reclaimed by the *stale* sweep (no
    /// heartbeat within `upstream_lease_stale_after_ms`) rather than by TTL
    /// expiry.  Kept separate from `leaked_reclaimed_total` so an operator can
    /// tell the two reclaim modes apart.  Always 0 on the Redis backend.
    pub stale_reclaimed_total: u64,
    /// C5.1: local backend only: leases currently held for this upstream whose
    /// last heartbeat is older than `upstream_lease_stale_after_ms` — i.e. how
    /// many slots the stale sweep would reclaim *right now*.  A non-zero value
    /// means slots are held by dead owners (leaked guards) rather than live
    /// in-flight traffic.  Always 0 on the Redis backend (its Lua sweeps
    /// self-heal on every reserve).
    pub stale_lease_count: u32,
    /// C5.1: local backend only: age in seconds of the oldest currently-held
    /// lease for this upstream (max `now - last_renewed_at`).  0 when no
    /// leases are held.  Interpreting it together with
    /// `upstream_lease_stale_after_ms` distinguishes "about to be swept"
    /// (age > stale_after) from "recently heartbeated and healthy".  Always 0
    /// on the Redis backend.
    pub oldest_lease_age_seconds: u64,
    /// C5.1: how many requests are currently queued behind the local
    /// concurrency gate (C3) for this upstream's accounts.  Process-local, so
    /// it is meaningful on both backends; the queue itself is a per-process
    /// in-memory structure.
    pub queue_depth: u32,
    /// E5.3: median observed lease hold duration across this upstream's
    /// accounts, milliseconds (E3 samples: release − reserve).  `None` until
    /// at least two samples exist — the honest "we don't know yet" (0 on the
    /// Redis backend, which keeps leases in Lua).
    pub hold_p50_ms: Option<u64>,
    /// E5.3: p95 observed lease hold duration, milliseconds (see
    /// `hold_p50_ms`).
    pub hold_p95_ms: Option<u64>,
    /// E5.3: cumulative local-gate (pre-dispatch slot) rejections for this
    /// upstream.  A non-zero value means the E4 safety net actually fired.
    /// Local backend only — the Redis backend enforces concurrency inside Lua.
    pub capacity_reject_total: u64,
    /// E5.3: cumulative count of E1 route/key cooldown skips (capacity-class
    /// failure recorded as observation only) for this upstream.  Local
    /// backend only (Redis route health lives in Lua).
    pub route_cooldown_skipped_total: u64,
}

impl UpstreamRuntimeSnapshot {
    pub fn is_in_cooldown(&self, now: u64) -> bool {
        self.cooldown_until > now
    }

    pub fn cooldown_remaining(&self, now: u64) -> u64 {
        self.cooldown_until.saturating_sub(now)
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct UpstreamRuntimeSnapshotWithFeedback {
    pub in_flight: u32,
    pub minute_cost: f64,
    pub five_hour_cost: f64,
    pub cooldown_until: u64,
    pub cooldown_remaining: u64,
    pub last_feedback_type: Option<String>,
    pub last_retry_after_seconds: Option<u64>,
    pub leaked_reclaimed_total: u64,
    /// C2.3: see `UpstreamRuntimeSnapshot::stale_reclaimed_total`.
    pub stale_reclaimed_total: u64,
    /// C5.1: see `UpstreamRuntimeSnapshot::stale_lease_count`.
    pub stale_lease_count: u32,
    /// C5.1: see `UpstreamRuntimeSnapshot::oldest_lease_age_seconds`.
    pub oldest_lease_age_seconds: u64,
    /// C5.1: see `UpstreamRuntimeSnapshot::queue_depth`.
    pub queue_depth: u32,
    /// E5.3: see `UpstreamRuntimeSnapshot::hold_p50_ms`.
    pub hold_p50_ms: Option<u64>,
    /// E5.3: see `UpstreamRuntimeSnapshot::hold_p95_ms`.
    pub hold_p95_ms: Option<u64>,
    /// E5.3: see `UpstreamRuntimeSnapshot::capacity_reject_total`.
    pub capacity_reject_total: u64,
    /// E5.3: see `UpstreamRuntimeSnapshot::route_cooldown_skipped_total`.
    pub route_cooldown_skipped_total: u64,
}

#[derive(Debug, Clone)]
pub struct ActiveGatewayRequestStart {
    pub request_id: String,
    pub downstream_id: String,
    pub downstream_name: String,
    pub endpoint: String,
    pub model: String,
    pub protocol: String,
    pub user_agent: Option<String>,
}

/// E5.1: one row of the retry-amplification metric — how many
/// client-visible retryable-capacity terminal errors (`upstream_routes_exhausted` /
/// `gateway_concurrency_saturated` / upstream 429) this `(downstream_id, model)`
/// pair received inside the requested window.  A `count` far above the real
/// request rate is the retry-loop tell.
#[derive(Debug, Clone, Serialize)]
pub struct RetryTerminalPoint {
    pub downstream_id: String,
    pub model: String,
    pub count: u64,
}

/// E5.1: default sliding-window length for the retry-amplification metric
/// (seconds).  Five minutes matches a client's typical fast retry loop while
/// staying cheap to keep in memory.
pub const RETRY_AMPLIFICATION_WINDOW_SECONDS: u64 = 300;

#[derive(Debug, Clone, Serialize)]
pub struct ActiveGatewayRequestSnapshot {
    pub request_id: String,
    pub downstream_id: String,
    pub downstream_name: String,
    pub endpoint: String,
    pub model: String,
    pub protocol: String,
    pub user_agent: Option<String>,
    pub upstream_id: Option<String>,
    pub upstream_name: Option<String>,
    pub started_at: u64,
    pub last_event_at: u64,
    pub elapsed_seconds: u64,
    pub idle_seconds: u64,
    pub status: String,
    pub error_category: Option<String>,
    /// E5.2: lifecycle phase — `selecting` / `queued_local` / `dispatched` /
    /// `streaming` / `awaiting_first_output`.  `status` stays the coarse
    /// routing/upstream/streaming/error label for backward compat; `phase`
    /// is the fine-grained view that distinguishes "queued for a local slot"
    /// from "still choosing a route".
    pub phase: String,
    /// E5.2: position in the C3 local-slot queue when `phase == queued_local`.
    pub queue_position: Option<usize>,
}

#[derive(Debug, Clone)]
struct ActiveGatewayRequest {
    request_id: String,
    downstream_id: String,
    downstream_name: String,
    endpoint: String,
    model: String,
    protocol: String,
    user_agent: Option<String>,
    upstream_id: Option<String>,
    upstream_name: Option<String>,
    started_at: u64,
    last_event_at: u64,
    status: String,
    error_category: Option<String>,
    /// E5.2: see `ActiveGatewayRequestSnapshot::phase`.
    phase: String,
    /// E5.2: see `ActiveGatewayRequestSnapshot::queue_position`.
    queue_position: Option<usize>,
}

#[derive(Debug, Clone)]
struct RoutingAffinityEntry {
    upstream_id: String,
    expires_at: u64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct QuotaEvent {
    pub(crate) created_at: u64,
    pub(crate) cost: f64,
}

#[derive(Debug, Clone)]
pub enum DownstreamAdmissionRejection {
    RuntimeCoordinationUnavailable,
    ConcurrencyLimitExceeded {
        retry_after_seconds: u64,
        limit: u32,
        /// Matched `model_concurrency_groups` entry name, `None` when the
        /// global `max_concurrency` backstop was hit (or no group matched).
        group: Option<String>,
    },
    PerMinuteLimitExceeded {
        retry_after_seconds: u64,
        limit: u32,
        used: u32,
    },
    RequestQuotaExceeded {
        retry_after_seconds: u64,
        limit: u32,
        used: u32,
        window_seconds: u64,
    },
    DailyCostQuotaExceeded {
        retry_after_seconds: u64,
        limit: u64,
        used: u64,
    },
}

impl DownstreamAdmissionRejection {
    pub fn retry_after_seconds(&self) -> u64 {
        match self {
            DownstreamAdmissionRejection::RuntimeCoordinationUnavailable => 1,
            DownstreamAdmissionRejection::ConcurrencyLimitExceeded {
                retry_after_seconds,
                ..
            }
            | DownstreamAdmissionRejection::PerMinuteLimitExceeded {
                retry_after_seconds,
                ..
            }
            | DownstreamAdmissionRejection::RequestQuotaExceeded {
                retry_after_seconds,
                ..
            }
            | DownstreamAdmissionRejection::DailyCostQuotaExceeded {
                retry_after_seconds,
                ..
            } => (*retry_after_seconds).max(1),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamAdmissionRejectionReason {
    LocalConcurrency,
    HedgeMinuteQuota,
    HedgeWindowQuota,
    RuntimeCoordinationUnavailable,
}

#[derive(Debug, Clone)]
pub struct UpstreamAdmissionError {
    pub message: String,
    pub retry_after_seconds: u64,
    pub reason: UpstreamAdmissionRejectionReason,
}

impl UpstreamAdmissionError {
    pub fn new(
        reason: UpstreamAdmissionRejectionReason,
        message: String,
        retry_after_seconds: u64,
    ) -> Self {
        Self {
            message,
            retry_after_seconds: retry_after_seconds.max(1),
            reason,
        }
    }

    fn runtime_coordination_unavailable() -> Self {
        Self {
            message: "runtime coordination unavailable".into(),
            retry_after_seconds: 1,
            reason: UpstreamAdmissionRejectionReason::RuntimeCoordinationUnavailable,
        }
    }

    pub fn is_runtime_coordination_unavailable(&self) -> bool {
        self.reason == UpstreamAdmissionRejectionReason::RuntimeCoordinationUnavailable
    }
}

fn quota_event_cost(events: &VecDeque<QuotaEvent>) -> f64 {
    events.iter().map(|event| event.cost).sum()
}

fn prune_quota_events(events: &mut VecDeque<QuotaEvent>, now: u64, window_seconds: u64) {
    let window_start = now.saturating_sub(window_seconds.saturating_sub(1));
    while let Some(event) = events.front() {
        if event.created_at < window_start {
            events.pop_front();
        } else {
            break;
        }
    }
}
// ============================================================================
// Public methods for managing upstreams and downstreams
// ============================================================================

impl AppState {
    /// Add a new upstream
    pub async fn add_upstream(&self, upstream: UpstreamConfig) -> Result<(), String> {
        let mut upstream = upstream;
        upstream.normalize_for_storage();
        upstream.validate_configuration()?;
        self.mutate_persisted_state(
            |state| {
                if state
                    .upstreams
                    .iter()
                    .any(|existing| existing.id == upstream.id)
                {
                    return Err(format!("Upstream with ID '{}' already exists", upstream.id));
                }
                Arc::make_mut(&mut state.upstreams).push(upstream);
                Ok(())
            },
            |error| format!("Failed to persist state: {error}"),
        )
        .await?;
        let current_upstreams = self.routing_snapshot().await.upstreams;
        self.reconcile_route_health(&current_upstreams)
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    /// Delete an upstream
    pub async fn delete_upstream_by_id(&self, id: &str) -> Result<(), String> {
        self.mutate_persisted_state(
            |state| {
                let initial_len = state.upstreams.len();
                Arc::make_mut(&mut state.upstreams).retain(|upstream| upstream.id != id);
                if state.upstreams.len() == initial_len {
                    return Err(format!("Upstream '{}' not found", id));
                }
                Ok(())
            },
            |error| format!("Failed to persist state: {error}"),
        )
        .await?;
        let current_upstreams = self.routing_snapshot().await.upstreams;
        self.reconcile_route_health(&current_upstreams)
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    /// Toggle upstream active status
    pub async fn toggle_upstream_by_id(&self, id: &str) -> Result<bool, String> {
        let active = self
            .mutate_persisted_state(
                |state| {
                    let upstream = Arc::make_mut(&mut state.upstreams)
                        .iter_mut()
                        .find(|upstream| upstream.id == id)
                        .ok_or_else(|| format!("Upstream '{}' not found", id))?;
                    upstream.active = !upstream.active;
                    Ok(upstream.active)
                },
                |error| format!("Failed to persist state: {error}"),
            )
            .await?;
        let current_upstreams = self.routing_snapshot().await.upstreams;
        self.reconcile_route_health(&current_upstreams)
            .await
            .map_err(|error| error.to_string())?;
        Ok(active)
    }

    /// Add a new downstream
    pub async fn add_downstream(&self, mut downstream: DownstreamConfig) -> Result<(), String> {
        validate_downstream_plaintext_pair(&mut downstream);
        let mut inner = self.inner.lock().await;
        if inner.downstreams.iter().any(|d| d.id == downstream.id) {
            return Err(format!(
                "Downstream with ID '{}' already exists",
                downstream.id
            ));
        }
        Arc::make_mut(&mut inner.downstreams).push(downstream);
        Ok(())
    }

    /// Update an existing downstream
    pub async fn update_downstream_by_id(
        &self,
        id: &str,
        updates: serde_json::Value,
    ) -> Result<DownstreamConfig, String> {
        self.mutate_persisted_state(
            |candidate_state| {
                let downstream = Arc::make_mut(&mut candidate_state.downstreams)
                    .iter_mut()
                    .find(|d| d.id == id)
                    .ok_or_else(|| format!("Downstream '{}' not found", id))?;

                if let Some(name) = updates.get("name").and_then(|v| v.as_str()) {
                    downstream.name = name.to_string();
                }
                if let Some(per_minute_limit) =
                    updates.get("per_minute_limit").and_then(|v| v.as_u64())
                {
                    downstream.per_minute_limit = per_minute_limit as u32;
                }
                if let Some(max_concurrency) =
                    updates.get("max_concurrency").and_then(|v| v.as_u64())
                {
                    downstream.max_concurrency = max_concurrency as u32;
                }
                if let Some(rate_limit_enabled) =
                    updates.get("rate_limit_enabled").and_then(|v| v.as_bool())
                {
                    downstream.rate_limit_enabled = rate_limit_enabled;
                }
                if let Some(request_quota_window_hours) = updates
                    .get("request_quota_window_hours")
                    .and_then(|v| v.as_u64())
                {
                    downstream.request_quota_window_hours = Some(request_quota_window_hours as u32);
                }
                if updates
                    .get("request_quota_window_hours")
                    .is_some_and(serde_json::Value::is_null)
                {
                    downstream.request_quota_window_hours = None;
                }
                if let Some(request_quota_requests) = updates
                    .get("request_quota_requests")
                    .and_then(|v| v.as_u64())
                {
                    downstream.request_quota_requests = Some(request_quota_requests as u32);
                }
                if updates
                    .get("request_quota_requests")
                    .is_some_and(serde_json::Value::is_null)
                {
                    downstream.request_quota_requests = None;
                }
                if let Some(model_allowlist) =
                    updates.get("model_allowlist").and_then(|v| v.as_array())
                {
                    downstream.model_allowlist = model_allowlist
                        .iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect();
                }
                if let Some(ip_allowlist) = updates.get("ip_allowlist").and_then(|v| v.as_array()) {
                    downstream.ip_allowlist = ip_allowlist
                        .iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect();
                }
                if let Some(active) = updates.get("active").and_then(|v| v.as_bool()) {
                    downstream.active = active;
                }

                Ok(downstream.clone())
            },
            |e| format!("Failed to persist state: {e}"),
        )
        .await
    }

    /// Delete a downstream
    pub async fn delete_downstream_by_id(&self, id: &str) -> Result<(), String> {
        let mut inner = self.inner.lock().await;
        let initial_len = inner.downstreams.len();
        Arc::make_mut(&mut inner.downstreams).retain(|d| d.id != id);
        if inner.downstreams.len() < initial_len {
            Ok(())
        } else {
            Err(format!("Downstream '{}' not found", id))
        }
    }

    /// Toggle downstream active status
    pub async fn toggle_downstream_by_id(&self, id: &str) -> Result<bool, String> {
        let mut inner = self.inner.lock().await;
        let downstream = Arc::make_mut(&mut inner.downstreams)
            .iter_mut()
            .find(|d| d.id == id)
            .ok_or_else(|| format!("Downstream '{}' not found", id))?;
        downstream.active = !downstream.active;
        Ok(downstream.active)
    }

    /// Update downstream hash (for key rotation)
    pub async fn update_downstream_hash(&self, id: &str, new_hash: String) -> Result<(), String> {
        let mut inner = self.inner.lock().await;
        let downstream = Arc::make_mut(&mut inner.downstreams)
            .iter_mut()
            .find(|d| d.id == id)
            .ok_or_else(|| format!("Downstream '{}' not found", id))?;
        downstream.hash = new_hash;
        validate_downstream_plaintext_pair(downstream);
        Ok(())
    }
}

const CAPABILITY_PROBE_BATCH_RETENTION_SECONDS: u64 = 2 * 60 * 60;
const CAPABILITY_PROBE_BATCH_MAX_RECORDS: usize = 256;

fn prune_capability_probe_batches(
    batches: &mut HashMap<String, CapabilityProbeBatchRecord>,
    now: u64,
) {
    let cutoff = now.saturating_sub(CAPABILITY_PROBE_BATCH_RETENTION_SECONDS);
    batches.retain(|_, record| {
        record
            .status
            .terminal_at
            .is_none_or(|terminal_at| terminal_at >= cutoff)
    });
    if batches.len() <= CAPABILITY_PROBE_BATCH_MAX_RECORDS {
        return;
    }
    let mut oldest = batches
        .iter()
        .map(|(id, record)| (id.clone(), record.status.started_at))
        .collect::<Vec<_>>();
    oldest.sort_by_key(|(_, started_at)| *started_at);
    for (id, _) in oldest.into_iter().take(
        batches
            .len()
            .saturating_sub(CAPABILITY_PROBE_BATCH_MAX_RECORDS),
    ) {
        batches.remove(&id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn routing_config_accessors_do_not_wait_for_usage_log_locks() {
        let state = AppState::new(
            PersistedState::default(),
            PathBuf::from("unused-state-path.json"),
            AppConfig::default(),
        );

        let pending_guard = state.pending_usage_logs.lock().await;
        let upstreams = tokio::time::timeout(Duration::from_millis(100), state.upstreams()).await;
        assert!(
            upstreams.is_ok(),
            "upstream config reads must not wait for pending usage logs"
        );
        drop(pending_guard);

        let archived_guard = state.archived_usage_logs.lock().await;
        let downstreams =
            tokio::time::timeout(Duration::from_millis(100), state.downstreams()).await;
        assert!(
            downstreams.is_ok(),
            "downstream config reads must not wait for archived usage logs"
        );
        drop(archived_guard);
    }

    fn test_downstream_config(id: &str) -> DownstreamConfig {
        DownstreamConfig {
            id: id.into(),
            name: "test downstream".into(),
            hash: String::new(),
            plaintext_key: None,
            plaintext_key_prefix: None,
            model_allowlist: vec![],
            rate_limit_enabled: true,
            per_minute_limit: 100,
            max_concurrency: 1,
            daily_token_limit: None,
            monthly_token_limit: None,
            input_token_price_per_million_cents: None,
            output_token_price_per_million_cents: None,
            daily_cost_limit_cents: None,
            request_quota_window_hours: None,
            request_quota_requests: None,
            ip_allowlist: vec![],
            expires_at: None,
            active: true,
            billing_mode: "request".into(),
            model_concurrency_groups: vec![],
        }
    }

    #[tokio::test]
    async fn local_downstream_leases_expire_and_are_pruned_from_snapshots() {
        let state = AppState::new(
            PersistedState::default(),
            PathBuf::from("unused-state-path.json"),
            AppConfig::default(),
        );
        let downstream = test_downstream_config("local-ttl-downstream");
        let now_ms = crate::util::unix_millis();
        {
            let mut runtime = state.downstream_runtime.lock().unwrap();
            let entry = runtime.entry(downstream.id.clone()).or_default();
            entry
                .admitted
                .insert("expired-lease".into(), now_ms - 1_000);
            entry.admitted.insert("live-lease".into(), now_ms + 600_000);
            entry.waiting.insert("expired-lease".into());
        }

        let counts = state
            .downstream_runtime_snapshot(&downstream)
            .await
            .unwrap();
        assert_eq!(counts.admitted, 1, "expired leases must be pruned");
        assert_eq!(
            counts.waiting_upstream, 0,
            "orphaned waiting entries must be pruned"
        );
        assert_eq!(counts.running, 1);

        let runtime = state.downstream_runtime.lock().unwrap();
        let entry = runtime.get(&downstream.id).unwrap();
        assert!(!entry.admitted.contains_key("expired-lease"));
        assert!(entry.admitted.contains_key("live-lease"));
        assert!(!entry.waiting.contains("expired-lease"));
    }

    #[tokio::test]
    async fn local_downstream_expired_leases_free_capacity_for_new_requests() {
        let state = AppState::new(
            PersistedState::default(),
            PathBuf::from("unused-state-path.json"),
            AppConfig::default(),
        );
        let downstream = test_downstream_config("local-ttl-capacity");
        let first = state
            .try_reserve_downstream_concurrency(&downstream, "test-model")
            .await
            .unwrap();
        assert!(
            state
                .try_reserve_downstream_concurrency(&downstream, "test-model")
                .await
                .is_err(),
            "second reservation must be rejected while the first lease is live"
        );
        {
            let mut runtime = state.downstream_runtime.lock().unwrap();
            let entry = runtime.get_mut(&downstream.id).unwrap();
            let lease_id = first.lease_id.as_ref().expect("local lease id").clone();
            entry
                .admitted
                .insert(lease_id, crate::util::unix_millis() - 1_000);
        }
        let second = state
            .try_reserve_downstream_concurrency(&downstream, "test-model")
            .await
            .unwrap();
        assert!(
            second.lease_id.is_some(),
            "expired lease must release capacity for a new reservation"
        );
    }

    #[tokio::test]
    async fn local_downstream_lease_renewal_extends_expiry() {
        let state = AppState::new(
            PersistedState::default(),
            PathBuf::from("unused-state-path.json"),
            AppConfig::default(),
        );
        let downstream = test_downstream_config("local-ttl-renewal");
        let lease = state
            .try_reserve_downstream_concurrency(&downstream, "test-model")
            .await
            .unwrap();
        let lease_id = lease.lease_id.as_ref().expect("local lease id").clone();
        {
            let mut runtime = state.downstream_runtime.lock().unwrap();
            let entry = runtime.get_mut(&downstream.id).unwrap();
            entry
                .admitted
                .insert(lease_id.clone(), crate::util::unix_millis() + 1_000);
        }
        state.renew_downstream_concurrency(&lease).await.unwrap();
        let runtime = state.downstream_runtime.lock().unwrap();
        let entry = runtime.get(&downstream.id).unwrap();
        let expires_at = *entry
            .admitted
            .get(&lease_id)
            .expect("renewal must keep the lease present");
        assert!(
            expires_at > crate::util::unix_millis() + 60_000,
            "renewal must push the expiry into the future"
        );
    }
}
