use chat_responses_codex::capabilities::AgentClientProfile;
use chat_responses_codex::server::{
    validate_client_json, validate_client_stream, SemanticExpectation,
};
use serde_json::{json, Value};

const CODEX_0_144_0_FIXTURE: &str = include_str!("fixtures/clients/codex-0.144.0-responses.json");
const OPENCODE_1_17_9_FIXTURE: &str = include_str!("fixtures/clients/opencode-1.17.9-chat.json");
const CLAUDE_CODE_2_1_195_FIXTURE: &str =
    include_str!("fixtures/clients/claude-code-2.1.195-messages.json");
const HERMES_0_14_0_FIXTURE: &str = include_str!("fixtures/clients/hermes-0.14.0-chat.json");

struct ClientFixtureExpectation<'a> {
    client: &'a str,
    version: &'a str,
    source_commit: &'a str,
    model: &'a str,
    path: &'a str,
    header_names: &'a [&'a str],
    protocol: &'a str,
}

fn validate_exact_client_fixture(
    fixture: &str,
    expected: &ClientFixtureExpectation<'_>,
) -> Result<(), String> {
    let value: Value = serde_json::from_str(fixture).map_err(|error| error.to_string())?;
    let object = value
        .as_object()
        .ok_or_else(|| "fixture must be an object".to_string())?;
    let mut top_level_keys = object.keys().map(String::as_str).collect::<Vec<_>>();
    top_level_keys.sort_unstable();
    if top_level_keys
        != [
            "_fixture_metadata",
            "body",
            "header_names",
            "method",
            "path",
        ]
    {
        return Err("fixture request envelope is incomplete".into());
    }
    let metadata = value
        .get("_fixture_metadata")
        .and_then(Value::as_object)
        .ok_or_else(|| "fixture metadata is missing".to_string())?;
    for (field, expected_value) in [
        ("client", expected.client),
        ("version", expected.version),
        ("source_commit", expected.source_commit),
    ] {
        if metadata.get(field).and_then(Value::as_str) != Some(expected_value) {
            return Err(format!("fixture metadata {field} mismatch"));
        }
    }
    if metadata.get("sanitized").and_then(Value::as_bool) != Some(true) {
        return Err("fixture must be marked sanitized".into());
    }
    if value.get("method").and_then(Value::as_str) != Some("POST") {
        return Err("fixture method must be POST".into());
    }
    if value.get("path").and_then(Value::as_str) != Some(expected.path) {
        return Err("fixture path mismatch".into());
    }
    let header_names = value
        .get("header_names")
        .and_then(Value::as_array)
        .ok_or_else(|| "fixture header_names is missing".to_string())?
        .iter()
        .map(|name| {
            name.as_str()
                .ok_or_else(|| "fixture header name must be a string".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    if header_names != expected.header_names {
        return Err("fixture header_names mismatch".into());
    }
    let body_value = value
        .get("body")
        .ok_or_else(|| "fixture body is missing".to_string())?;
    let body = body_value
        .as_object()
        .ok_or_else(|| "fixture body is missing".to_string())?;
    if body.get("model").and_then(Value::as_str) != Some(expected.model)
        || body.get("stream").and_then(Value::as_bool) != Some(true)
    {
        return Err("fixture body must use the synthetic streaming model shape".into());
    }
    let prompt = match expected.protocol {
        "responses" => body.get("input").and_then(Value::as_str),
        "chat" | "messages" => body_value
            .pointer("/messages/0/content")
            .and_then(Value::as_str),
        _ => return Err("unknown fixture protocol".into()),
    };
    if prompt != Some("synthetic matrix request") {
        return Err("fixture prompt is not sanitized".into());
    }
    match expected.protocol {
        "responses" => {
            if body_value.pointer("/tools/0/name").and_then(Value::as_str)
                != Some("gateway_matrix_probe")
            {
                return Err("Responses fixture tool shape mismatch".into());
            }
        }
        "chat" => {
            if body_value
                .pointer("/tools/0/function/name")
                .and_then(Value::as_str)
                != Some("gateway_matrix_probe")
            {
                return Err("Chat fixture tool shape mismatch".into());
            }
        }
        "messages" => {
            if body_value.pointer("/thinking/type").and_then(Value::as_str) != Some("adaptive")
                || body_value
                    .pointer("/output_config/effort")
                    .and_then(Value::as_str)
                    != Some("high")
                || body_value.pointer("/tools/0/name").and_then(Value::as_str)
                    != Some("gateway_matrix_probe")
                || body_value.pointer("/tools/0/input_schema").is_none()
            {
                return Err("Messages fixture body shape mismatch".into());
            }
        }
        _ => unreachable!(),
    }
    let serialized_body = Value::Object(body.clone()).to_string();
    for forbidden in [
        "Bearer ",
        "sk-",
        "SECRET_",
        "data:image",
        "reasoning-marker",
        "file contents",
        "README.md",
    ] {
        if serialized_body.contains(forbidden) {
            return Err(format!("fixture contains unsanitized content: {forbidden}"));
        }
    }
    Ok(())
}

fn exact_client_fixture_cases() -> [(&'static str, ClientFixtureExpectation<'static>); 4] {
    [
        (
            CODEX_0_144_0_FIXTURE,
            ClientFixtureExpectation {
                client: "codex",
                version: "0.144.0",
                source_commit: "767822446c7a594caa19609ca435281a9ec67e0d",
                model: "synthetic-model",
                path: "/v1/responses",
                header_names: &["authorization", "content-type", "user-agent"],
                protocol: "responses",
            },
        ),
        (
            OPENCODE_1_17_9_FIXTURE,
            ClientFixtureExpectation {
                client: "opencode",
                version: "1.17.9",
                source_commit: "5c23e88419c4743b9be42cea132f2fb1e6cb63ff",
                model: "synthetic-model",
                path: "/v1/chat/completions",
                header_names: &["authorization", "content-type", "user-agent"],
                protocol: "chat",
            },
        ),
        (
            CLAUDE_CODE_2_1_195_FIXTURE,
            ClientFixtureExpectation {
                client: "claude_code",
                version: "2.1.195",
                source_commit: "be02c39841a59e2ac1f35ac12285def02acdbb5a",
                model: "opaque-public",
                path: "/v1/messages?beta=true",
                header_names: &[
                    "x-api-key",
                    "anthropic-version",
                    "anthropic-beta",
                    "content-type",
                    "user-agent",
                ],
                protocol: "messages",
            },
        ),
        (
            HERMES_0_14_0_FIXTURE,
            ClientFixtureExpectation {
                client: "hermes",
                version: "0.14.0",
                source_commit: "43e566f77eaf01293086eb7cb99a21e240d60634",
                model: "synthetic-model",
                path: "/v1/chat/completions",
                header_names: &["authorization", "content-type", "user-agent"],
                protocol: "chat",
            },
        ),
    ]
}

#[test]
fn exact_version_client_fixtures_have_sanitized_request_envelopes() {
    for (fixture, expected) in exact_client_fixture_cases() {
        validate_exact_client_fixture(fixture, &expected).unwrap_or_else(|error| {
            panic!("{} {} fixture: {error}", expected.client, expected.version)
        });
    }
}

#[test]
fn exact_version_client_fixture_validation_rejects_missing_or_unsanitized_structure() {
    let (_, expected) = exact_client_fixture_cases().into_iter().next().unwrap();
    let mut missing_body: Value = serde_json::from_str(CODEX_0_144_0_FIXTURE).unwrap();
    missing_body.as_object_mut().unwrap().remove("body");
    assert!(validate_exact_client_fixture(&missing_body.to_string(), &expected).is_err());

    let mut unsanitized: Value = serde_json::from_str(CODEX_0_144_0_FIXTURE).unwrap();
    unsanitized["body"]["input"] = Value::String("SECRET_real_prompt".into());
    assert!(validate_exact_client_fixture(&unsanitized.to_string(), &expected).is_err());
}

fn valid_responses_fixture_with_namespace_reasoning() -> Vec<u8> {
    br#"{
        "id": "resp_1",
        "object": "response",
        "status": "completed",
        "output": [
            {
                "id": "rs_1",
                "type": "reasoning",
                "content": [{"type": "reasoning_text", "text": "reasoning-marker-17"}]
            },
            {
                "id": "fc_1",
                "type": "function_call",
                "call_id": "call_1",
                "name": "spawn_agent",
                "arguments": "{\"nonce\":\"1\"}",
                "namespace": "multi_agent_v1"
            }
        ]
    }"#
    .to_vec()
}

#[test]
fn http_200_empty_or_malformed_stream_is_not_a_pass() {
    for (profile, fixture) in [
        (
            AgentClientProfile::Codex,
            include_str!("fixtures/clients/malformed-responses.sse"),
        ),
        (
            AgentClientProfile::Opencode,
            include_str!("fixtures/clients/malformed-chat.sse"),
        ),
        (
            AgentClientProfile::ClaudeCode,
            include_str!("fixtures/clients/malformed-messages.sse"),
        ),
    ] {
        let result =
            validate_client_stream(profile, fixture.as_bytes(), &SemanticExpectation::text());
        assert!(!result.passed, "{profile:?} malformed stream passed");
        assert!(result
            .codes
            .iter()
            .any(|code| code.starts_with("missing_") || code.starts_with("invalid_")));
        assert_eq!(
            result.error_category.as_deref(),
            Some("gateway_protocol_semantic_invalid"),
            "{profile:?} changed the category of a stream without a gateway error frame"
        );
    }
}

#[test]
fn chat_done_without_text_reasoning_or_tool_delta_is_not_meaningful() {
    let result = validate_client_stream(
        AgentClientProfile::Opencode,
        b"data: [DONE]\n\n",
        &SemanticExpectation::text(),
    );

    assert!(!result.passed);
    assert!(result
        .codes
        .iter()
        .any(|code| code == "missing_meaningful_event"));
}

#[test]
fn chat_stream_requires_the_forced_function_and_parseable_fragmented_arguments() {
    let plain_text = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"not a tool\"}}]}\n\n",
        "data: [DONE]\n\n"
    );
    let expected = SemanticExpectation::forced_function("gateway_matrix_probe");
    let rejected =
        validate_client_stream(AgentClientProfile::Hermes, plain_text.as_bytes(), &expected);
    assert!(!rejected.passed);
    assert!(rejected
        .codes
        .iter()
        .any(|code| code == "missing_forced_function"));

    let fragmented = concat!(
        "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"synthetic-model\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"gateway_matrix_probe\",\"arguments\":\"{\\\"nonce\\\":\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"synthetic-model\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"n-1\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    assert!(
        validate_client_stream(AgentClientProfile::Hermes, fragmented.as_bytes(), &expected,)
            .passed
    );
}

#[test]
fn responses_stream_requires_output_items_before_completion() {
    let completed_only = concat!(
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"output\":[]}}\n\n"
    );
    let result = validate_client_stream(
        AgentClientProfile::Codex,
        completed_only.as_bytes(),
        &SemanticExpectation::text(),
    );
    assert!(!result.passed);
    assert!(result
        .codes
        .iter()
        .any(|code| code == "missing_meaningful_event"));
}

#[test]
fn responses_empty_delta_is_not_a_meaningful_event() {
    let stream = concat!(
        "event: response.output_text.delta\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"\"}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"output\":[]}}\n\n"
    );
    let result = validate_client_stream(
        AgentClientProfile::Codex,
        stream.as_bytes(),
        &SemanticExpectation::text(),
    );

    assert!(!result.passed);
    assert!(result
        .codes
        .iter()
        .any(|code| code == "missing_meaningful_event"));
}

#[test]
fn responses_stream_requires_matching_completed_event_and_item_ids() {
    let wrong_event = concat!(
        "event: response.output_item.added\n",
        "data: {\"type\":\"response.output_item.added\",\"item\":{\"id\":\"msg_1\",\"type\":\"message\"}}\n\n",
        "event: response.output_text.delta\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"OK\"}\n\n",
        "event: response.incomplete\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"output\":[]}}\n\n"
    );
    let result = validate_client_stream(
        AgentClientProfile::Codex,
        wrong_event.as_bytes(),
        &SemanticExpectation::text(),
    );
    assert!(!result.passed);
    assert!(result
        .codes
        .iter()
        .any(|code| code == "missing_response_completed"));

    let missing_item = concat!(
        "event: response.output_text.delta\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"OK\"}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"output\":[]}}\n\n"
    );
    let result = validate_client_stream(
        AgentClientProfile::Codex,
        missing_item.as_bytes(),
        &SemanticExpectation::text(),
    );
    assert!(!result.passed);
    assert!(result
        .codes
        .iter()
        .any(|code| code == "missing_output_item_id"));
}

#[test]
fn chat_stream_requires_the_exact_finish_reason() {
    let stream = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"OK\"}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"length\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    let result = validate_client_stream(
        AgentClientProfile::Hermes,
        stream.as_bytes(),
        &SemanticExpectation::text(),
    );

    assert!(!result.passed);
    assert!(result
        .codes
        .iter()
        .any(|code| code == "invalid_finish_reason"));
}

#[test]
fn chat_stream_requires_official_chunk_envelopes_and_choice_indices() {
    let stream = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"OK\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n"
    );

    let result = validate_client_stream(
        AgentClientProfile::Opencode,
        stream.as_bytes(),
        &SemanticExpectation::text(),
    );

    assert!(!result.passed);
    assert!(result
        .codes
        .iter()
        .any(|code| code == "invalid_chat_chunk_envelope"));
}

#[test]
fn chat_stream_accepts_official_terminal_usage_only_chunk() {
    let stream = concat!(
        "data: {\"id\":\"chatcmpl-usage\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"synthetic-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"OK\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-usage\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"synthetic-model\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: {\"id\":\"chatcmpl-usage\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"synthetic-model\",\"choices\":[],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\n\n",
        "data: [DONE]\n\n"
    );

    let result = validate_client_stream(
        AgentClientProfile::Opencode,
        stream.as_bytes(),
        &SemanticExpectation::text(),
    );

    assert!(result.passed, "codes were {:?}", result.codes);
}

#[test]
fn chat_stream_rejects_choice_chunk_after_terminal_finish() {
    let stream = concat!(
        "data: {\"id\":\"chatcmpl-late\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"synthetic-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"OK\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-late\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"synthetic-model\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: {\"id\":\"chatcmpl-late\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"synthetic-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"late\"},\"finish_reason\":null}]}\n\n",
        "data: [DONE]\n\n"
    );

    let result = validate_client_stream(
        AgentClientProfile::Opencode,
        stream.as_bytes(),
        &SemanticExpectation::text(),
    );

    assert!(!result.passed);
    assert!(result
        .codes
        .iter()
        .any(|code| code == "invalid_terminal_order"));
}

#[test]
fn chat_stream_rejects_malformed_data_and_invalid_done_order() {
    let malformed = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"OK\"}}]}\n\n",
        "data: {not-json}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    let malformed_result = validate_client_stream(
        AgentClientProfile::Opencode,
        malformed.as_bytes(),
        &SemanticExpectation::text(),
    );
    assert!(!malformed_result.passed);
    assert!(malformed_result
        .codes
        .iter()
        .any(|code| code == "invalid_sse_data"));

    let finish_after_done = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"OK\"}}]}\n\n",
        "data: [DONE]\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n"
    );
    let order_result = validate_client_stream(
        AgentClientProfile::Hermes,
        finish_after_done.as_bytes(),
        &SemanticExpectation::text(),
    );
    assert!(!order_result.passed);
    assert!(order_result
        .codes
        .iter()
        .any(|code| code == "invalid_done_order"));

    let duplicate_done = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"OK\"}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
        "data: [DONE]\n\n"
    );
    let duplicate_result = validate_client_stream(
        AgentClientProfile::Opencode,
        duplicate_done.as_bytes(),
        &SemanticExpectation::text(),
    );
    assert!(!duplicate_result.passed);
    assert!(duplicate_result
        .codes
        .iter()
        .any(|code| code == "invalid_done_count"));
}

#[test]
fn forced_tool_stream_plain_text_is_a_model_compatibility_failure() {
    let stream = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"I cannot call it\"},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    let result = validate_client_stream(
        AgentClientProfile::Opencode,
        stream.as_bytes(),
        &SemanticExpectation::forced_function("gateway_matrix_probe"),
    );

    assert_eq!(
        result.error_category.as_deref(),
        Some("gateway_model_semantic_incompatible")
    );
}

#[test]
fn claude_empty_text_delta_is_not_a_meaningful_event() {
    let stream = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\"}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
    );
    let result = validate_client_stream(
        AgentClientProfile::ClaudeCode,
        stream.as_bytes(),
        &SemanticExpectation::text(),
    );

    assert!(!result.passed);
    assert!(result
        .codes
        .iter()
        .any(|code| code == "missing_content_block_delta"));
}

#[test]
fn claude_stream_requires_the_exact_stop_reason() {
    let stream = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\"}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"OK\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"max_tokens\"}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
    );
    let result = validate_client_stream(
        AgentClientProfile::ClaudeCode,
        stream.as_bytes(),
        &SemanticExpectation::text(),
    );

    assert!(!result.passed);
    assert!(result
        .codes
        .iter()
        .any(|code| code == "invalid_stop_reason"));
}

#[test]
fn claude_stream_binds_event_types_deltas_and_one_signature_per_thinking_block() {
    let forged_type = concat!(
        "event: message_start\ndata: {\"type\":\"forged\",\"message\":{\"id\":\"msg_1\"}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"OK\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
    );
    let forged_result = validate_client_stream(
        AgentClientProfile::ClaudeCode,
        forged_type.as_bytes(),
        &SemanticExpectation::text(),
    );
    assert!(!forged_result.passed);
    assert!(forged_result
        .codes
        .iter()
        .any(|code| code == "invalid_messages_event_type"));

    let thinking_in_text = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\"}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"marker-17\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig-1\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
    );
    let text_result = validate_client_stream(
        AgentClientProfile::ClaudeCode,
        thinking_in_text.as_bytes(),
        &SemanticExpectation {
            expected_reasoning_marker: Some("marker-17".into()),
            ..SemanticExpectation::text()
        },
    );
    assert!(!text_result.passed);
    assert!(text_result
        .codes
        .iter()
        .any(|code| code == "invalid_content_block_delta"));

    let duplicate_signature = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\"}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"marker-17\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig-1\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig-2\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
    );
    let signature_result = validate_client_stream(
        AgentClientProfile::ClaudeCode,
        duplicate_signature.as_bytes(),
        &SemanticExpectation {
            expected_reasoning_marker: Some("marker-17".into()),
            ..SemanticExpectation::text()
        },
    );
    assert!(!signature_result.passed);
    assert!(signature_result
        .codes
        .iter()
        .any(|code| code == "invalid_thinking_signature"));
}

#[test]
fn successful_json_requires_protocol_specific_meaningful_output() {
    for (profile, body) in [
        (
            AgentClientProfile::Codex,
            br#"{"id":"resp_1","object":"response","status":"completed","output":[]}"#
                .as_slice(),
        ),
        (
            AgentClientProfile::Opencode,
            br#"{"choices":[{"message":{"role":"assistant","content":""},"finish_reason":"stop"}]}"#
                .as_slice(),
        ),
        (
            AgentClientProfile::ClaudeCode,
            br#"{"id":"msg_1","type":"message","role":"assistant","content":[],"stop_reason":"end_turn"}"#
                .as_slice(),
        ),
    ] {
        let result = validate_client_json(profile, body, &SemanticExpectation::text());
        assert!(!result.passed, "{profile:?} accepted empty JSON output");
        assert!(result.codes.iter().any(|code| code == "missing_meaningful_output"));
    }
}

#[test]
fn responses_json_requires_envelope_item_ids_and_typed_content_parts() {
    for (body, expected_code) in [
        (
            br#"{
                "object":"response","status":"completed",
                "output":[{"id":"msg_1","type":"message","content":[{
                    "type":"output_text","text":"OK"
                }]}]
            }"#
            .as_slice(),
            "invalid_response_envelope",
        ),
        (
            br#"{
                "id":"resp_1","object":"response","status":"completed",
                "output":[{"type":"message","content":[{
                    "type":"output_text","text":"OK"
                }]}]
            }"#
            .as_slice(),
            "missing_output_item_id",
        ),
        (
            br#"{
                "id":"resp_1","object":"response","status":"completed",
                "output":[{"id":"msg_1","type":"message","content":[{
                    "type":"input_text","text":"OK"
                }]}]
            }"#
            .as_slice(),
            "invalid_output_content_type",
        ),
    ] {
        let result = validate_client_json(
            AgentClientProfile::Codex,
            body,
            &SemanticExpectation::text(),
        );
        assert!(!result.passed, "invalid Responses JSON passed: {result:?}");
        assert!(
            result.codes.iter().any(|code| code == expected_code),
            "missing {expected_code}: {result:?}"
        );
    }
}

#[test]
fn json_terminal_reasons_must_match_the_observed_output_kind() {
    for (profile, body, code) in [
        (
            AgentClientProfile::Opencode,
            br#"{"choices":[{"message":{"role":"assistant","content":"OK"},"finish_reason":"length"}]}"#
                .as_slice(),
            "invalid_finish_reason",
        ),
        (
            AgentClientProfile::ClaudeCode,
            br#"{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"OK"}],"stop_reason":"max_tokens"}"#
                .as_slice(),
            "invalid_stop_reason",
        ),
    ] {
        let result = validate_client_json(profile, body, &SemanticExpectation::text());
        assert!(!result.passed, "{profile:?} accepted the wrong terminal reason");
        assert!(result.codes.iter().any(|observed| observed == code));
    }
}

#[test]
fn forced_tool_plain_text_is_a_model_failure_for_responses_and_messages() {
    for (profile, body) in [
        (
            AgentClientProfile::Codex,
            br#"{"id":"resp_1","object":"response","status":"completed","output":[{"id":"msg_1","type":"message","content":[{"type":"output_text","text":"I cannot call it"}]}]}"#
                .as_slice(),
        ),
        (
            AgentClientProfile::ClaudeCode,
            br#"{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"I cannot call it"}],"stop_reason":"end_turn"}"#
                .as_slice(),
        ),
    ] {
        let result = validate_client_json(
            profile,
            body,
            &SemanticExpectation::forced_function("gateway_matrix_probe"),
        );
        assert_eq!(
            result.error_category.as_deref(),
            Some("gateway_model_semantic_incompatible"),
            "{profile:?} returned the wrong error category: {result:?}"
        );
    }
}

#[test]
fn image_semantic_check_requires_the_expected_label() {
    let expected = SemanticExpectation {
        expected_image_label: Some("red-square".into()),
        ..SemanticExpectation::text()
    };
    for (profile, body) in [
        (
            AgentClientProfile::Codex,
            br#"{"id":"resp_1","object":"response","status":"completed","output":[{"id":"msg_1","type":"message","status":"completed","role":"assistant","content":[{"type":"output_text","text":"blue-square"}]}]}"#
                .as_slice(),
        ),
        (
            AgentClientProfile::Opencode,
            br#"{"choices":[{"message":{"role":"assistant","content":"blue-square"},"finish_reason":"stop"}]}"#
                .as_slice(),
        ),
        (
            AgentClientProfile::ClaudeCode,
            br#"{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"blue-square"}],"stop_reason":"end_turn"}"#
                .as_slice(),
        ),
    ] {
        let result = validate_client_json(profile, body, &expected);
        assert!(!result.passed, "{profile:?} accepted the wrong image label");
        assert!(result.codes.iter().any(|code| code == "missing_expected_image_label"));
    }
}

#[test]
fn image_label_and_reasoning_marker_must_match_exactly() {
    let image = br#"{
        "choices":[{"message":{"role":"assistant","content":"prefix-OK-suffix"},"finish_reason":"stop"}]
    }"#;
    let image_result = validate_client_json(
        AgentClientProfile::Hermes,
        image,
        &SemanticExpectation {
            expected_image_label: Some("OK".into()),
            ..SemanticExpectation::text()
        },
    );
    assert!(!image_result.passed);
    assert!(image_result
        .codes
        .iter()
        .any(|code| code == "missing_expected_image_label"));

    let reasoning = br#"{
        "id":"resp_1","object":"response","status":"completed",
        "output":[{"id":"rs_1","type":"reasoning","content":[{
            "type":"reasoning_text","text":"prefix-marker-17-suffix"
        }]}]
    }"#;
    let reasoning_result = validate_client_json(
        AgentClientProfile::Codex,
        reasoning,
        &SemanticExpectation {
            expected_reasoning_marker: Some("marker-17".into()),
            ..SemanticExpectation::text()
        },
    );
    assert!(!reasoning_result.passed);
    assert!(reasoning_result
        .codes
        .iter()
        .any(|code| code == "missing_reasoning_marker"));
}

#[test]
fn claude_stream_enforces_event_order_and_thinking_signature_delta() {
    let expected = SemanticExpectation {
        expected_reasoning_marker: Some("reasoning-marker-17".into()),
        ..SemanticExpectation::text()
    };
    let missing_signature = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\"}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"reasoning-marker-17\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
    );
    let result = validate_client_stream(
        AgentClientProfile::ClaudeCode,
        missing_signature.as_bytes(),
        &expected,
    );
    assert!(!result.passed);
    assert!(result
        .codes
        .iter()
        .any(|code| code == "missing_thinking_signature"));
}

#[test]
fn plain_text_to_forced_tool_prompt_is_model_compatibility_failure() {
    let body =
        br#"{"choices":[{"message":{"content":"I would call the tool"},"finish_reason":"stop"}]}"#;
    let result = validate_client_json(
        AgentClientProfile::Hermes,
        body,
        &SemanticExpectation::forced_function("gateway_matrix_probe"),
    );
    assert_eq!(
        result.error_category.as_deref(),
        Some("gateway_model_semantic_incompatible")
    );
}

#[test]
fn responses_namespace_and_reasoning_markers_must_be_restored() {
    let body = valid_responses_fixture_with_namespace_reasoning();
    let expected = SemanticExpectation::codex_namespace_reasoning(
        "multi_agent_v1",
        "spawn_agent",
        "reasoning-marker-17",
    );
    assert!(validate_client_json(AgentClientProfile::Codex, &body, &expected).passed);
}

#[test]
fn forced_tool_response_requires_a_linkable_call_id() {
    let body = br#"{
        "id":"resp_1","object":"response","status":"completed",
        "output":[{"id":"fc_1","type":"function_call","name":"gateway_matrix_probe","arguments":"{}"}]
    }"#;
    let result = validate_client_json(
        AgentClientProfile::Codex,
        body,
        &SemanticExpectation::forced_function("gateway_matrix_probe"),
    );

    assert!(!result.passed);
    assert!(result
        .codes
        .iter()
        .any(|code| code == "missing_linked_continuation"));
}

#[test]
fn malformed_usage_is_not_semantically_valid_when_present() {
    let body = br#"{
        "choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],
        "usage":{"prompt_tokens":5,"completion_tokens":4,"total_tokens":2}
    }"#;
    let result = validate_client_json(
        AgentClientProfile::Opencode,
        body,
        &SemanticExpectation::text(),
    );

    assert!(!result.passed);
    assert!(result.codes.iter().any(|code| code == "invalid_usage"));
}

#[test]
fn anthropic_error_event_fails_even_with_valid_stream_framing() {
    let stream = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\"}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\"}}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
    );
    let result = validate_client_stream(
        AgentClientProfile::ClaudeCode,
        stream.as_bytes(),
        &SemanticExpectation::text(),
    );

    assert!(!result.passed);
    assert!(result
        .codes
        .iter()
        .any(|code| code == "messages_error_event"));
    assert_eq!(
        result.error_category.as_deref(),
        Some("gateway_protocol_semantic_invalid")
    );
}

#[test]
fn structured_gateway_sse_errors_preserve_their_category_for_every_client_protocol() {
    let category = "upstream_stream_error_event";
    let openai_error = concat!(
        "data: {\"error\":{\"message\":\"upstream stream failed\",",
        "\"type\":\"upstream_error\",\"code\":\"upstream_stream_error_event\",",
        "\"category\":\"upstream_stream_error_event\",\"details\":{\"scope\":\"upstream\"}}}\n\n",
        "data: [DONE]\n\n"
    );
    let messages_error = concat!(
        "event: error\n",
        "data: {\"type\":\"error\",\"error\":{\"message\":\"upstream stream failed\",",
        "\"type\":\"api_error\",\"code\":\"upstream_stream_error_event\",",
        "\"category\":\"upstream_stream_error_event\",\"details\":{\"scope\":\"upstream\"}}}\n\n"
    );

    for (profile, stream, expected_semantic_code) in [
        (
            AgentClientProfile::Codex,
            openai_error,
            "missing_response_completed",
        ),
        (
            AgentClientProfile::Opencode,
            openai_error,
            "invalid_chat_chunk_envelope",
        ),
        (
            AgentClientProfile::Hermes,
            openai_error,
            "invalid_chat_chunk_envelope",
        ),
        (
            AgentClientProfile::ClaudeCode,
            messages_error,
            "messages_error_event",
        ),
    ] {
        let result =
            validate_client_stream(profile, stream.as_bytes(), &SemanticExpectation::text());

        assert!(!result.passed, "{profile:?} accepted a gateway error frame");
        assert_eq!(
            result.error_category.as_deref(),
            Some(category),
            "{profile:?} discarded the structured gateway error category: {result:?}"
        );
        assert!(
            result
                .codes
                .iter()
                .any(|code| code == expected_semantic_code),
            "{profile:?} discarded its semantic failure codes: {result:?}"
        );
    }
}

#[test]
fn incomplete_error_shapes_are_not_trusted_as_gateway_categories() {
    let stream = concat!(
        "event: error\n",
        "data: {\"type\":\"error\",\"error\":{",
        "\"type\":\"overloaded_error\",\"category\":\"model_supplied_category\"}}\n\n"
    );
    let result = validate_client_stream(
        AgentClientProfile::ClaudeCode,
        stream.as_bytes(),
        &SemanticExpectation::text(),
    );

    assert!(!result.passed);
    assert_eq!(
        result.error_category.as_deref(),
        Some("gateway_protocol_semantic_invalid")
    );
    assert!(result
        .codes
        .iter()
        .any(|code| code == "messages_error_event"));
}

#[test]
fn model_supplied_error_objects_cannot_forge_gateway_categories() {
    for error in [
        json!({
            "message": "model supplied",
            "type": "upstream_error",
            "code": "different_code",
            "category": "forged_category",
            "details": {"scope": "upstream"}
        }),
        json!({
            "message": "model supplied",
            "type": "assistant_output",
            "code": "forged_category",
            "category": "forged_category",
            "details": {"scope": "upstream"}
        }),
        json!({
            "message": "model supplied",
            "type": "upstream_error",
            "code": "forged_category",
            "category": "forged_category",
            "details": {"scope": "model"}
        }),
    ] {
        let stream = format!("data: {}\n\ndata: [DONE]\n\n", json!({"error": error}));
        let result = validate_client_stream(
            AgentClientProfile::Opencode,
            stream.as_bytes(),
            &SemanticExpectation::text(),
        );

        assert!(!result.passed);
        assert_eq!(
            result.error_category.as_deref(),
            Some("gateway_protocol_semantic_invalid"),
            "trusted a non-gateway error object: {result:?}"
        );
    }
}
