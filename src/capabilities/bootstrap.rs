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
