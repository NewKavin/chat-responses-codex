mod policy;
mod profile;
mod probe_queue;
mod resolver;
mod types;

pub use policy::{CapabilityPolicyError, CompiledCapabilityConfiguration, CompiledExpectation};
pub use profile::{
    apply_probe_outcome, normalize_route_base_url, profile_is_current, route_fingerprint,
    ProbeOutcome, RouteFingerprintInput,
};
pub use probe_queue::{ProbeJob, ProbeQueueState, ProbeReason};
pub use resolver::{CapabilityResolutionError, CapabilityResolver, ResolutionInput};
pub use types::*;

pub const CAPABILITY_SCHEMA_VERSION: u32 = 1;
pub const DIALECT_PROBE_SCHEMA_VERSION: u32 = 1;
pub const MAX_CAPABILITY_COLLECTION_ENTRIES: usize = 1_024;
pub const MAX_CAPABILITY_CONFIGURATION_BYTES: usize = 1_048_576;
pub const MAX_CAPABILITY_EXTENSION_CASE_BYTES: usize = 16_384;
pub const MAX_CAPABILITY_EXTENSION_PROBES: usize = 1_024;
pub const MAX_CAPABILITY_SELECTOR_VALUE_BYTES: usize = 1_024;
