mod policy;
mod types;

pub use policy::{CapabilityPolicyError, CompiledCapabilityConfiguration, CompiledExpectation};
pub use types::*;

pub const CAPABILITY_SCHEMA_VERSION: u32 = 1;
pub const DIALECT_PROBE_SCHEMA_VERSION: u32 = 1;
pub const MAX_CAPABILITY_COLLECTION_ENTRIES: usize = 1_024;
pub const MAX_CAPABILITY_CONFIGURATION_BYTES: usize = 1_048_576;
pub const MAX_CAPABILITY_EXTENSION_CASE_BYTES: usize = 16_384;
pub const MAX_CAPABILITY_EXTENSION_PROBES: usize = 1_024;
pub const MAX_CAPABILITY_SELECTOR_VALUE_BYTES: usize = 1_024;
