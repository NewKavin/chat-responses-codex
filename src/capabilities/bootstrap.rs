use super::CapabilityConfiguration;

const DEPLOYMENT_POLICY_JSON: &str =
    include_str!("../../templates/capabilities/current-deployment.example.json");

pub fn deployment_capability_configuration() -> Result<CapabilityConfiguration, String> {
    let configuration: CapabilityConfiguration =
        serde_json::from_str(DEPLOYMENT_POLICY_JSON).map_err(|error| error.to_string())?;
    configuration
        .clone()
        .compile()
        .map_err(|error| error.to_string())?;
    if configuration.revision == 0 || configuration.policies.is_empty() {
        return Err("embedded deployment capability policy is empty".into());
    }
    Ok(configuration)
}

/// Id prefixes that mark a template entry as builtin-owned and therefore
/// eligible for append-only merge into operator-managed configurations.
/// Entries with these prefixes are never used to overwrite or delete an
/// existing entry; they are only appended when missing.
pub const BUILTIN_POLICY_ID_PREFIXES: &[&str] = &["domestic-"];

fn is_builtin_entry_id(id: &str) -> bool {
    BUILTIN_POLICY_ID_PREFIXES
        .iter()
        .any(|prefix| id.starts_with(prefix))
}

/// Appends builtin-owned template entries that are missing from `stored`
/// (append-only merge). Operator entries are never replaced or removed.
/// Returns the ids of appended entries. Bundles referenced by newly added
/// expectations are appended as well when missing.
pub fn merge_builtin_policy_entries(
    stored: &mut CapabilityConfiguration,
    template: &CapabilityConfiguration,
) -> Vec<String> {
    let mut added = Vec::new();
    for policy in &template.policies {
        if is_builtin_entry_id(&policy.id)
            && !stored
                .policies
                .iter()
                .any(|existing| existing.id == policy.id)
        {
            stored.policies.push(policy.clone());
            added.push(policy.id.clone());
        }
    }
    let mut added_expectations = Vec::new();
    for expectation in &template.compatibility_expectations {
        if is_builtin_entry_id(&expectation.id)
            && !stored
                .compatibility_expectations
                .iter()
                .any(|existing| existing.id == expectation.id)
        {
            stored.compatibility_expectations.push(expectation.clone());
            added.push(expectation.id.clone());
            added_expectations.push(expectation);
        }
    }
    let referenced_bundles = added_expectations
        .iter()
        .flat_map(|expectation| expectation.bundles.iter().map(String::as_str))
        .collect::<std::collections::BTreeSet<_>>();
    for bundle in &template.bundles {
        if referenced_bundles.contains(bundle.id.as_str())
            && !stored
                .bundles
                .iter()
                .any(|existing| existing.id == bundle.id)
        {
            stored.bundles.push(bundle.clone());
            added.push(bundle.id.clone());
        }
    }
    added
}
