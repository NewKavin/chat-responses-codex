use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use tokio::time::Instant;

use super::{Capability, DialectProfileKey};

pub const RUNTIME_CAPABILITY_HINT_CAPACITY: usize = 16_384;
pub const RUNTIME_CAPABILITY_HINT_TTL: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum CapabilityHintDiscriminator {
    Feature {
        capability: Capability,
        value: Option<String>,
    },
    Protocol,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CapabilityHintKey {
    pub profile: DialectProfileKey,
    pub discriminator: CapabilityHintDiscriminator,
}

impl CapabilityHintKey {
    pub fn feature(
        profile: DialectProfileKey,
        capability: Capability,
        value: Option<String>,
    ) -> Self {
        Self {
            profile,
            discriminator: CapabilityHintDiscriminator::Feature { capability, value },
        }
    }

    pub fn protocol(profile: DialectProfileKey) -> Self {
        Self {
            profile,
            discriminator: CapabilityHintDiscriminator::Protocol,
        }
    }
}

#[derive(Clone, Debug)]
struct RuntimeCapabilityHintEntry {
    configuration_fingerprint: String,
    expires_at: Instant,
}

#[derive(Clone, Debug, Default)]
pub struct RuntimeCapabilityHintSnapshot {
    entries: HashMap<CapabilityHintKey, String>,
}

impl RuntimeCapabilityHintSnapshot {
    pub fn blocks_protocol(
        &self,
        profile: &DialectProfileKey,
        configuration_fingerprint: &str,
    ) -> bool {
        self.entries.iter().any(|(key, fingerprint)| {
            key.profile == *profile
                && fingerprint == configuration_fingerprint
                && matches!(key.discriminator, CapabilityHintDiscriminator::Protocol)
        })
    }

    pub fn blocked_features(
        &self,
        profile: &DialectProfileKey,
        configuration_fingerprint: &str,
        requested_value: Option<&str>,
    ) -> Vec<(Capability, Option<&str>)> {
        self.entries
            .iter()
            .filter_map(|(key, fingerprint)| {
                if key.profile != *profile || fingerprint != configuration_fingerprint {
                    return None;
                }
                match &key.discriminator {
                    CapabilityHintDiscriminator::Feature { capability, value }
                        if value
                            .as_deref()
                            .is_none_or(|value| Some(value) == requested_value) =>
                    {
                        Some((*capability, value.as_deref()))
                    }
                    _ => None,
                }
            })
            .collect()
    }
}

pub struct RuntimeCapabilityHints {
    entries: HashMap<CapabilityHintKey, RuntimeCapabilityHintEntry>,
    capacity: usize,
    ttl: Duration,
}

impl RuntimeCapabilityHints {
    pub fn new(capacity: usize, ttl: Duration) -> Self {
        Self {
            entries: HashMap::new(),
            capacity: capacity.max(1),
            ttl: ttl.max(Duration::from_secs(1)),
        }
    }

    pub fn insert(&mut self, key: CapabilityHintKey, configuration_fingerprint: String) -> bool {
        let now = Instant::now();
        self.prune_expired(now);
        if let Some(entry) = self.entries.get_mut(&key) {
            let changed = entry.configuration_fingerprint != configuration_fingerprint;
            entry.configuration_fingerprint = configuration_fingerprint;
            entry.expires_at = now + self.ttl;
            return changed;
        }
        if self.entries.len() >= self.capacity {
            if let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.expires_at)
                .map(|(key, _)| key.clone())
            {
                self.entries.remove(&oldest);
            }
        }
        self.entries.insert(
            key,
            RuntimeCapabilityHintEntry {
                configuration_fingerprint,
                expires_at: now + self.ttl,
            },
        );
        true
    }

    pub fn is_active(&mut self, key: &CapabilityHintKey, configuration_fingerprint: &str) -> bool {
        let now = Instant::now();
        self.prune_expired(now);
        let matches = self
            .entries
            .get(key)
            .is_some_and(|entry| entry.configuration_fingerprint == configuration_fingerprint);
        if !matches
            && self
                .entries
                .get(key)
                .is_some_and(|entry| entry.configuration_fingerprint != configuration_fingerprint)
        {
            self.entries.remove(key);
        }
        matches
    }

    pub fn remove(&mut self, key: &CapabilityHintKey) -> bool {
        self.entries.remove(key).is_some()
    }

    pub fn clear_profile(&mut self, profile: &DialectProfileKey, configuration_fingerprint: &str) {
        self.entries.retain(|key, entry| {
            key.profile != *profile || entry.configuration_fingerprint != configuration_fingerprint
        });
    }

    pub fn clear_features_for_success(
        &mut self,
        profile: &DialectProfileKey,
        configuration_fingerprint: &str,
        capabilities: &BTreeSet<Capability>,
        requested_value: Option<&str>,
    ) {
        self.entries.retain(|key, entry| {
            if key.profile != *profile
                || entry.configuration_fingerprint != configuration_fingerprint
            {
                return true;
            }
            match &key.discriminator {
                CapabilityHintDiscriminator::Feature { capability, value }
                    if capabilities.contains(capability) =>
                {
                    value.as_deref() != requested_value
                }
                _ => true,
            }
        });
    }

    pub fn clear_after_conclusive_probe(
        &mut self,
        profile: &DialectProfileKey,
        configuration_fingerprint: &str,
        capabilities: &BTreeSet<Capability>,
    ) {
        self.entries.retain(|key, entry| {
            if key.profile != *profile
                || entry.configuration_fingerprint != configuration_fingerprint
            {
                return true;
            }
            match &key.discriminator {
                CapabilityHintDiscriminator::Protocol => false,
                CapabilityHintDiscriminator::Feature { capability, .. } => {
                    !capabilities.contains(capability)
                }
            }
        });
    }

    pub fn retain_current<F>(&mut self, mut is_current: F)
    where
        F: FnMut(&CapabilityHintKey, &str) -> bool,
    {
        self.prune_expired(Instant::now());
        self.entries
            .retain(|key, entry| is_current(key, &entry.configuration_fingerprint));
    }

    pub fn snapshot(&mut self) -> RuntimeCapabilityHintSnapshot {
        self.prune_expired(Instant::now());
        RuntimeCapabilityHintSnapshot {
            entries: self
                .entries
                .iter()
                .map(|(key, entry)| (key.clone(), entry.configuration_fingerprint.clone()))
                .collect(),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    fn prune_expired(&mut self, now: Instant) {
        self.entries.retain(|_, entry| entry.expires_at > now);
    }
}

impl Default for RuntimeCapabilityHints {
    fn default() -> Self {
        Self::new(
            RUNTIME_CAPABILITY_HINT_CAPACITY,
            RUNTIME_CAPABILITY_HINT_TTL,
        )
    }
}
