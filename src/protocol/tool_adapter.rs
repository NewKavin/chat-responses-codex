use super::ProtocolError;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    Function,
    Custom,
    NamespaceMember,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct ToolIdentity {
    pub kind: ToolKind,
    pub namespace: Option<String>,
    pub name: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ToolMapping {
    pub identity: ToolIdentity,
    pub upstream_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ToolAdapterRegistry {
    pub version: u32,
    pub mappings: Vec<ToolMapping>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolTarget {
    NativeResponses,
    RestrictedResponses,
    FunctionsOnly,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ToolAdaptation {
    pub upstream_tools: Vec<Value>,
    pub registry: ToolAdapterRegistry,
    pub downgrades: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ToolPolicyDecision {
    Keep,
    DropOptional { downgrade: String },
    Reject { category: &'static str },
}

pub fn hosted_tool_decision(
    kind: &str,
    route_supports_kind: bool,
    explicitly_selected: bool,
    executable_tool_count_after_drop: usize,
) -> ToolPolicyDecision {
    let known = matches!(kind, "web_search" | "file_search" | "computer_use");
    if !known {
        return ToolPolicyDecision::Reject {
            category: "gateway_protocol_capability_unsupported",
        };
    }
    if route_supports_kind {
        return ToolPolicyDecision::Keep;
    }
    if explicitly_selected || executable_tool_count_after_drop == 0 {
        ToolPolicyDecision::Reject {
            category: "gateway_protocol_capability_unsupported",
        }
    } else {
        ToolPolicyDecision::DropOptional {
            downgrade: format!("optional_tool:{kind}"),
        }
    }
}

pub trait ReversibleToolAdapter: Sized {
    fn build(tools: &Value, target: ToolTarget) -> Result<ToolAdaptation, ProtocolError>;
    fn from_identities(identities: Vec<ToolIdentity>) -> Result<Self, ProtocolError>;
    fn adapt_tool_choice(&self, choice: &Value) -> Result<Value, ProtocolError>;
    fn restore_function_call(&self, call: &Value) -> Result<Value, ProtocolError>;
    fn adapt_call_output(&self, output: &Value) -> Result<Value, ProtocolError>;
    fn restore_streamed_call_name(
        &self,
        upstream_name: &str,
    ) -> Result<&ToolIdentity, ProtocolError>;
    fn upstream_name(&self, identity: &ToolIdentity) -> Option<&str>;
    fn identity(&self, upstream_name: &str) -> Option<&ToolIdentity>;
}

impl ToolIdentity {
    pub fn function(name: &str) -> Self {
        Self {
            kind: ToolKind::Function,
            namespace: None,
            name: name.to_owned(),
        }
    }

    pub fn namespace(namespace: &str, name: &str) -> Self {
        Self {
            kind: ToolKind::NamespaceMember,
            namespace: Some(namespace.to_owned()),
            name: name.to_owned(),
        }
    }

    pub fn custom(namespace: Option<&str>, name: &str) -> Self {
        Self {
            kind: ToolKind::Custom,
            namespace: namespace.map(str::to_owned),
            name: name.to_owned(),
        }
    }
}

impl ToolAdapterRegistry {
    pub const VERSION: u32 = 1;

    pub fn empty() -> Self {
        Self {
            version: Self::VERSION,
            mappings: Vec::new(),
        }
    }

    pub fn build(tools: &Value, target: ToolTarget) -> Result<ToolAdaptation, ProtocolError> {
        <Self as ReversibleToolAdapter>::build(tools, target)
    }

    pub fn from_identities(identities: Vec<ToolIdentity>) -> Result<Self, ProtocolError> {
        <Self as ReversibleToolAdapter>::from_identities(identities)
    }

    pub fn adapt_tool_choice(&self, choice: &Value) -> Result<Value, ProtocolError> {
        <Self as ReversibleToolAdapter>::adapt_tool_choice(self, choice)
    }

    pub fn restore_function_call(&self, call: &Value) -> Result<Value, ProtocolError> {
        <Self as ReversibleToolAdapter>::restore_function_call(self, call)
    }

    pub fn adapt_call_output(&self, output: &Value) -> Result<Value, ProtocolError> {
        <Self as ReversibleToolAdapter>::adapt_call_output(self, output)
    }

    pub fn restore_streamed_call_name(
        &self,
        upstream_name: &str,
    ) -> Result<&ToolIdentity, ProtocolError> {
        <Self as ReversibleToolAdapter>::restore_streamed_call_name(self, upstream_name)
    }

    pub fn upstream_name(&self, identity: &ToolIdentity) -> Option<&str> {
        <Self as ReversibleToolAdapter>::upstream_name(self, identity)
    }

    pub fn identity(&self, upstream_name: &str) -> Option<&ToolIdentity> {
        <Self as ReversibleToolAdapter>::identity(self, upstream_name)
    }

    fn mapping_for_identity(&self, identity: &ToolIdentity) -> Option<&ToolMapping> {
        self.mappings
            .iter()
            .find(|mapping| &mapping.identity == identity)
    }

    fn mapping_for_name(&self, upstream_name: &str) -> Option<&ToolMapping> {
        self.mappings
            .iter()
            .find(|mapping| mapping.upstream_name == upstream_name)
    }

    fn canonical_sort_key(identity: &ToolIdentity) -> (u8, Option<&str>, &str) {
        let kind_rank = match identity.kind {
            ToolKind::Function => 0,
            ToolKind::Custom => 1,
            ToolKind::NamespaceMember => 2,
        };
        (
            kind_rank,
            identity.namespace.as_deref(),
            identity.name.as_str(),
        )
    }

    fn assigned_upstream_name(
        identity: &ToolIdentity,
        occupied: &BTreeSet<String>,
    ) -> Result<String, ProtocolError> {
        let preserve_original = matches!(identity.kind, ToolKind::Function | ToolKind::Custom)
            && !occupied.contains(&identity.name);
        if preserve_original {
            return Ok(identity.name.clone());
        }

        let mut suffix_len = 12usize;
        while suffix_len <= 56 {
            let candidate = generated_name(identity, suffix_len, occupied);
            if !occupied.contains(&candidate) {
                return Ok(candidate);
            }
            suffix_len = (suffix_len + 4).min(56);
            if suffix_len == 56 {
                let candidate = generated_name(identity, suffix_len, occupied);
                if !occupied.contains(&candidate) {
                    return Ok(candidate);
                }
                break;
            }
        }

        Err(ProtocolError::InvalidPayload(format!(
            "tool name collision could not be resolved for {}",
            identity.name
        )))
    }

    fn custom_input_tool(name: String, description: Option<String>) -> Value {
        let mut function = Map::new();
        function.insert("name".into(), Value::String(name));
        if let Some(description) = description {
            function.insert("description".into(), Value::String(description));
        }
        function.insert(
            "parameters".into(),
            json!({
                "type": "object",
                "properties": {
                    "input": {"type": "string"}
                },
                "required": ["input"],
                "additionalProperties": false
            }),
        );

        json!({"type":"function","function": Value::Object(function)})
    }

    fn chat_function_tool(
        name: String,
        description: Option<String>,
        parameters: Option<Value>,
    ) -> Value {
        let mut function = Map::new();
        function.insert("name".into(), Value::String(name));
        if let Some(description) = description {
            function.insert("description".into(), Value::String(description));
        }
        if let Some(parameters) = parameters {
            function.insert("parameters".into(), parameters);
        }

        json!({"type":"function","function": Value::Object(function)})
    }

    fn parse_function_definition(
        tool: &Value,
    ) -> Result<(String, Option<String>, Option<Value>), ProtocolError> {
        let object = tool.as_object().ok_or_else(|| {
            ProtocolError::InvalidPayload(format!("unsupported tool definition: {tool}"))
        })?;

        if let Some(function) = object.get("function").and_then(Value::as_object) {
            let name = function
                .get("name")
                .and_then(Value::as_str)
                .ok_or(ProtocolError::MissingField("name"))?
                .to_string();
            let description = function
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| {
                    object
                        .get("description")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                });
            let parameters = function.get("parameters").cloned();
            return Ok((name, description, parameters));
        }

        let name = object
            .get("name")
            .and_then(Value::as_str)
            .ok_or(ProtocolError::MissingField("name"))?
            .to_string();
        let description = object
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let parameters = object.get("parameters").cloned();
        Ok((name, description, parameters))
    }

    fn parse_namespace_tool(
        tool: &Value,
        target: ToolTarget,
        identities: &mut Vec<ToolIdentity>,
        flattened_identities: &mut Vec<ToolIdentity>,
        flattened_tools: &mut Vec<Value>,
    ) -> Result<(), ProtocolError> {
        let object = tool.as_object().ok_or_else(|| {
            ProtocolError::InvalidPayload(format!("unsupported namespace tool: {tool}"))
        })?;
        let namespace = object
            .get("name")
            .and_then(Value::as_str)
            .ok_or(ProtocolError::MissingField("name"))?
            .to_string();
        let namespace_description = object
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let members = object
            .get("tools")
            .and_then(Value::as_array)
            .ok_or(ProtocolError::MissingField("tools"))?;

        for member in members {
            let member_object = member.as_object().ok_or_else(|| {
                ProtocolError::InvalidPayload(format!("unsupported namespace member: {member}"))
            })?;
            let kind = member_object
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("function");
            match kind {
                "function" => {
                    let (member_name, member_description, parameters) =
                        Self::parse_function_definition(member)?;
                    let identity = ToolIdentity::namespace(&namespace, &member_name);
                    identities.push(identity.clone());
                    if !matches!(target, ToolTarget::NativeResponses) {
                        let description = prefix_description(
                            namespace_description.as_deref(),
                            member_description.as_deref(),
                        );
                        flattened_identities.push(identity.clone());
                        flattened_tools.push(Self::chat_function_tool(
                            member_name,
                            description,
                            parameters,
                        ));
                    }
                }
                "custom" => {
                    let member_name = member_object
                        .get("name")
                        .and_then(Value::as_str)
                        .ok_or(ProtocolError::MissingField("name"))?
                        .to_string();
                    let identity = ToolIdentity::custom(Some(&namespace), &member_name);
                    identities.push(identity.clone());
                    if !matches!(target, ToolTarget::NativeResponses) {
                        let description = prefix_description(
                            namespace_description.as_deref(),
                            member_object.get("description").and_then(Value::as_str),
                        );
                        flattened_identities.push(identity.clone());
                        flattened_tools
                            .push(Self::custom_input_tool(identity.name.clone(), description));
                    }
                }
                other => {
                    return Err(ProtocolError::InvalidPayload(format!(
                        "unsupported namespace member type: {other}"
                    )));
                }
            }
        }

        Ok(())
    }

    fn parse_tool(
        tool: &Value,
        target: ToolTarget,
        identities: &mut Vec<ToolIdentity>,
        flattened_identities: &mut Vec<ToolIdentity>,
        flattened_tools: &mut Vec<Value>,
        downgrades: &mut Vec<String>,
    ) -> Result<(), ProtocolError> {
        let object = tool.as_object().ok_or_else(|| {
            ProtocolError::InvalidPayload(format!("unsupported tool definition: {tool}"))
        })?;

        match object
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("function")
        {
            "function" => {
                let (name, description, parameters) = Self::parse_function_definition(tool)?;
                let identity = ToolIdentity::function(&name);
                identities.push(identity.clone());
                if !matches!(target, ToolTarget::NativeResponses) {
                    flattened_identities.push(identity.clone());
                    flattened_tools.push(Self::chat_function_tool(name, description, parameters));
                }
            }
            "custom" => {
                let name = object
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or(ProtocolError::MissingField("name"))?
                    .to_string();
                let identity =
                    ToolIdentity::custom(object.get("namespace").and_then(Value::as_str), &name);
                identities.push(identity.clone());
                if !matches!(target, ToolTarget::NativeResponses) {
                    let description = object
                        .get("description")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    flattened_identities.push(identity.clone());
                    flattened_tools
                        .push(Self::custom_input_tool(identity.name.clone(), description));
                }
            }
            "namespace" => {
                Self::parse_namespace_tool(
                    tool,
                    target,
                    identities,
                    flattened_identities,
                    flattened_tools,
                )?;
            }
            kind if matches!(kind, "web_search" | "file_search" | "computer_use") => {
                let decision = hosted_tool_decision(
                    kind,
                    matches!(target, ToolTarget::NativeResponses),
                    false,
                    0,
                );
                match decision {
                    ToolPolicyDecision::Keep => {
                        if !matches!(target, ToolTarget::NativeResponses) {
                            flattened_tools.push(tool.clone());
                        }
                    }
                    ToolPolicyDecision::DropOptional { downgrade } => {
                        downgrades.push(downgrade);
                    }
                    ToolPolicyDecision::Reject { category } => {
                        return Err(ProtocolError::InvalidPayload(format!(
                            "{category}: hosted tool {kind} is not supported"
                        )));
                    }
                }
            }
            other => {
                return Err(ProtocolError::InvalidPayload(format!(
                    "unsupported tool type: {other}"
                )));
            }
        }

        Ok(())
    }
}

impl ReversibleToolAdapter for ToolAdapterRegistry {
    fn build(tools: &Value, target: ToolTarget) -> Result<ToolAdaptation, ProtocolError> {
        let array = tools.as_array().ok_or_else(|| {
            ProtocolError::InvalidPayload(format!("unsupported tool list: {tools}"))
        })?;

        let mut identities = Vec::new();
        let mut flattened_identities = Vec::new();
        let mut flattened_tools = Vec::new();
        let mut downgrades = Vec::new();

        for tool in array {
            Self::parse_tool(
                tool,
                target,
                &mut identities,
                &mut flattened_identities,
                &mut flattened_tools,
                &mut downgrades,
            )?;
        }

        let registry = Self::from_identities(identities)?;
        if !matches!(target, ToolTarget::NativeResponses) {
            for (identity, tool) in flattened_identities.iter().zip(flattened_tools.iter_mut()) {
                let Some(object) = tool.as_object_mut() else {
                    continue;
                };
                let Some(function) = object.get_mut("function").and_then(Value::as_object_mut)
                else {
                    continue;
                };
                if let Some(upstream_name) = registry.upstream_name(identity) {
                    function.insert("name".into(), Value::String(upstream_name.to_string()));
                }
            }
        }

        Ok(ToolAdaptation {
            upstream_tools: if matches!(target, ToolTarget::NativeResponses) {
                array.clone()
            } else {
                flattened_tools
            },
            registry,
            downgrades,
        })
    }

    fn from_identities(identities: Vec<ToolIdentity>) -> Result<Self, ProtocolError> {
        let mut identities = identities;
        identities.sort_by(|left, right| {
            let left_key = Self::canonical_sort_key(left);
            let right_key = Self::canonical_sort_key(right);
            left_key.cmp(&right_key)
        });
        identities.dedup();

        let mut occupied = BTreeSet::new();
        let mut mappings = Vec::with_capacity(identities.len());

        for identity in identities {
            let upstream_name = Self::assigned_upstream_name(&identity, &occupied)?;
            occupied.insert(upstream_name.clone());
            mappings.push(ToolMapping {
                identity,
                upstream_name,
            });
        }

        Ok(Self {
            version: Self::VERSION,
            mappings,
        })
    }

    fn adapt_tool_choice(&self, choice: &Value) -> Result<Value, ProtocolError> {
        match choice {
            Value::String(_) => Ok(choice.clone()),
            Value::Object(object) => {
                let mut adapted = object.clone();
                if let Some(name) = tool_choice_name(object) {
                    if let Some(identity) = self.identity(name) {
                        if let Some(upstream_name) = self.upstream_name(identity) {
                            set_tool_choice_name(&mut adapted, upstream_name);
                        }
                    }
                }
                Ok(Value::Object(adapted))
            }
            other => Err(ProtocolError::InvalidPayload(format!(
                "unsupported tool_choice value: {other}"
            ))),
        }
    }

    fn restore_function_call(&self, call: &Value) -> Result<Value, ProtocolError> {
        let object = call.as_object().ok_or_else(|| {
            ProtocolError::InvalidPayload(format!("unsupported function call: {call}"))
        })?;
        let (upstream_name, arguments) = extract_call_details(object)?;
        let call_id = object
            .get("id")
            .or_else(|| object.get("call_id"))
            .and_then(Value::as_str)
            .unwrap_or(&upstream_name);
        let status = object
            .get("status")
            .cloned()
            .unwrap_or_else(|| Value::String("completed".into()));

        if let Some(identity) = self.identity(&upstream_name) {
            return match identity.kind {
                ToolKind::Function => Ok(json!({
                    "type": "function_call",
                    "id": call_id,
                    "call_id": call_id,
                    "name": identity.name,
                    "arguments": arguments,
                    "status": status,
                })),
                ToolKind::NamespaceMember => Ok(json!({
                    "type": "function_call",
                    "id": call_id,
                    "call_id": call_id,
                    "name": identity.name,
                    "namespace": identity.namespace,
                    "arguments": arguments,
                    "status": status,
                })),
                ToolKind::Custom => Ok(json!({
                    "type": "custom_tool_call",
                    "id": call_id,
                    "call_id": call_id,
                    "name": identity.name,
                    "namespace": identity.namespace,
                    "input": extract_custom_input(&arguments)?,
                    "status": status,
                })),
            };
        }

        Ok(json!({
            "type": "function_call",
            "id": call_id,
            "call_id": call_id,
            "name": upstream_name,
            "arguments": arguments,
            "status": status,
        }))
    }

    fn adapt_call_output(&self, output: &Value) -> Result<Value, ProtocolError> {
        let object = output.as_object().ok_or_else(|| {
            ProtocolError::InvalidPayload(format!("unsupported call output: {output}"))
        })?;
        let mut adapted = object.clone();
        match object.get("type").and_then(Value::as_str) {
            Some("custom_tool_call_output") => {
                adapted.insert("type".into(), Value::String("function_call_output".into()));
                Ok(Value::Object(adapted))
            }
            Some("function_call_output") | None => Ok(Value::Object(adapted)),
            Some(other) => Err(ProtocolError::InvalidPayload(format!(
                "unsupported call output type: {other}"
            ))),
        }
    }

    fn restore_streamed_call_name(
        &self,
        upstream_name: &str,
    ) -> Result<&ToolIdentity, ProtocolError> {
        self.identity(upstream_name).ok_or_else(|| {
            ProtocolError::InvalidPayload(format!("unknown upstream tool name: {upstream_name}"))
        })
    }

    fn upstream_name(&self, identity: &ToolIdentity) -> Option<&str> {
        self.mapping_for_identity(identity)
            .map(|mapping| mapping.upstream_name.as_str())
    }

    fn identity(&self, upstream_name: &str) -> Option<&ToolIdentity> {
        self.mapping_for_name(upstream_name)
            .map(|mapping| &mapping.identity)
    }
}

fn prefix_description(
    namespace_description: Option<&str>,
    member_description: Option<&str>,
) -> Option<String> {
    match (namespace_description, member_description) {
        (Some(namespace), Some(member)) if !namespace.is_empty() && !member.is_empty() => {
            Some(format!("{namespace}: {member}"))
        }
        (Some(namespace), None) if !namespace.is_empty() => Some(namespace.to_owned()),
        (None, Some(member)) if !member.is_empty() => Some(member.to_owned()),
        (Some(namespace), Some(member)) if !member.is_empty() => {
            Some(format!("{namespace}: {member}"))
        }
        _ => None,
    }
}

fn tool_choice_name(object: &Map<String, Value>) -> Option<&str> {
    object
        .get("function")
        .and_then(Value::as_object)
        .and_then(|function| function.get("name"))
        .or_else(|| object.get("name"))
        .and_then(Value::as_str)
}

fn set_tool_choice_name(object: &mut Map<String, Value>, name: &str) {
    if let Some(function) = object.get_mut("function").and_then(Value::as_object_mut) {
        function.insert("name".into(), Value::String(name.to_owned()));
        return;
    }
    object.insert("name".into(), Value::String(name.to_owned()));
}

fn extract_call_details(object: &Map<String, Value>) -> Result<(String, String), ProtocolError> {
    if let Some(function) = object.get("function").and_then(Value::as_object) {
        let name = function
            .get("name")
            .and_then(Value::as_str)
            .ok_or(ProtocolError::MissingField("name"))?
            .to_string();
        let arguments = function
            .get("arguments")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        return Ok((name, arguments));
    }

    let name = object
        .get("name")
        .and_then(Value::as_str)
        .ok_or(ProtocolError::MissingField("name"))?
        .to_string();
    let arguments = object
        .get("arguments")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    Ok((name, arguments))
}

fn extract_custom_input(arguments: &str) -> Result<String, ProtocolError> {
    let value: Value = serde_json::from_str(arguments).map_err(|error| {
        ProtocolError::InvalidPayload(format!("invalid custom tool arguments: {error}"))
    })?;
    let input = value
        .as_object()
        .and_then(|object| object.get("input"))
        .and_then(Value::as_str)
        .ok_or(ProtocolError::MissingField("input"))?;
    Ok(input.to_string())
}

fn sanitize_middle(identity: &ToolIdentity) -> String {
    let raw = match identity.namespace.as_deref() {
        Some(namespace) => format!("{namespace}__{}", identity.name),
        None => identity.name.clone(),
    };
    let mut output = String::new();
    let mut last_was_separator = false;

    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            output.push(ch);
            last_was_separator = false;
        } else if !last_was_separator {
            output.push('_');
            last_was_separator = true;
        }
    }

    let trimmed = output.trim_matches(|ch: char| ch == '_' || ch == '-');
    if trimmed.is_empty() {
        "tool".to_string()
    } else {
        trimmed.to_string()
    }
}

fn identity_bytes(identity: &ToolIdentity) -> Vec<u8> {
    let kind = match identity.kind {
        ToolKind::Function => "function",
        ToolKind::Custom => "custom",
        ToolKind::NamespaceMember => "namespace_member",
    };
    let namespace = identity.namespace.as_deref().unwrap_or("");
    [
        kind.as_bytes(),
        b"\0",
        namespace.as_bytes(),
        b"\0",
        identity.name.as_bytes(),
    ]
    .concat()
}

pub fn generated_name(
    identity: &ToolIdentity,
    digest_len: usize,
    occupied: &BTreeSet<String>,
) -> String {
    let digest = Sha256::digest(identity_bytes(identity));
    let digest = format!("{:x}", digest);
    let mut suffix_len = digest_len.clamp(12, 56);
    let sanitized = sanitize_middle(identity);

    loop {
        let current_suffix_len = suffix_len.min(digest.len());
        let suffix = &digest[..current_suffix_len];
        let max_middle_len = 64usize.saturating_sub(4 + suffix.len());
        let mut middle = sanitized.clone();
        if middle.len() > max_middle_len {
            middle.truncate(max_middle_len);
            while middle.ends_with('_') || middle.ends_with('-') {
                middle.pop();
            }
            if middle.is_empty() {
                middle = "tool".to_string();
            }
        }

        let candidate = format!("gw_{middle}_{suffix}");
        if !occupied.contains(&candidate) || current_suffix_len >= 56 {
            return candidate;
        }
        suffix_len = (current_suffix_len + 4).min(56);
    }
}
