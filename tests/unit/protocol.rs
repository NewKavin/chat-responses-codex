use chat_responses_codex::protocol::tool_adapter::{ToolAdapterRegistry, ToolIdentity, ToolTarget};
use chat_responses_codex::protocol::StreamTranslator;
use chat_responses_codex::routing::UpstreamProtocol;
use serde_json::json;

#[test]
fn responses_stream_translator_ignores_reasoning_items_with_completed_usage() {
    let mut translator = StreamTranslator::new(
        UpstreamProtocol::Responses,
        UpstreamProtocol::ChatCompletions,
    )
    .expect("translator should exist");

    let reasoning_added = json!({
        "type": "response.output_item.added",
        "response_id": "resp-1",
        "output_index": 0,
        "item": {
            "id": "reasoning-1",
            "type": "reasoning",
            "status": "in_progress"
        }
    });
    translator
        .translate_event(&reasoning_added)
        .expect("reasoning item should not break stream translation");

    let text_delta = json!({
        "type": "response.output_text.delta",
        "response_id": "resp-1",
        "item_id": "msg-1",
        "output_index": 1,
        "content_index": 0,
        "delta": "Hello"
    });
    let text_chunks = translator
        .translate_event(&text_delta)
        .expect("text delta should translate");
    assert!(text_chunks.iter().any(|chunk| {
        chunk["choices"][0]["delta"]["content"]
            .as_str()
            .is_some_and(|content| content == "Hello")
    }));

    let completed = json!({
        "type": "response.completed",
        "response_id": "resp-1",
        "response": {
            "id": "resp-1",
            "object": "response",
            "created_at": 1,
            "status": "completed",
            "model": "gpt-4.1-mini",
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5,
                "total_tokens": 15
            },
            "output": [
                {
                    "id": "reasoning-1",
                    "type": "reasoning",
                    "status": "completed"
                },
                {
                    "id": "msg-1",
                    "type": "message",
                    "status": "completed",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": "Hello",
                        "annotations": []
                    }]
                }
            ]
        }
    });
    let final_chunks = translator
        .translate_event(&completed)
        .expect("completed usage event should not break stream translation");
    assert!(final_chunks.iter().any(|chunk| {
        chunk["choices"][0]["finish_reason"]
            .as_str()
            .is_some_and(|reason| reason == "stop")
    }));
}

#[test]
fn responses_stream_translator_rejects_unknown_output_item_types_on_added_events() {
    let mut translator = StreamTranslator::new(
        UpstreamProtocol::Responses,
        UpstreamProtocol::ChatCompletions,
    )
    .expect("translator should exist");

    let event = json!({
        "type": "response.output_item.added",
        "response_id": "resp-1",
        "output_index": 0,
        "item": {
            "id": "item-1",
            "type": "unsupported_output",
            "status": "in_progress"
        }
    });

    let error = translator
        .translate_event(&event)
        .expect_err("translation should fail");
    assert!(error
        .to_string()
        .contains("unsupported responses output item type"));
}

#[test]
fn responses_stream_translator_rejects_unknown_output_item_types_on_done_events() {
    let mut translator = StreamTranslator::new(
        UpstreamProtocol::Responses,
        UpstreamProtocol::ChatCompletions,
    )
    .expect("translator should exist");

    let event = json!({
        "type": "response.output_item.done",
        "response_id": "resp-1",
        "output_index": 0,
        "item": {
            "id": "item-1",
            "type": "unsupported_output",
            "status": "completed"
        }
    });

    let error = translator
        .translate_event(&event)
        .expect_err("translation should fail");
    assert!(error
        .to_string()
        .contains("unsupported responses output item type"));
}

#[test]
fn responses_stream_translator_rejects_non_assistant_output_roles() {
    let mut translator = StreamTranslator::new(
        UpstreamProtocol::Responses,
        UpstreamProtocol::ChatCompletions,
    )
    .expect("translator should exist");

    let event = json!({
        "type": "response.output_item.done",
        "response_id": "resp-1",
        "output_index": 0,
        "item": {
            "id": "msg-1",
            "type": "message",
            "status": "completed",
            "role": "user",
            "content": [{
                "type": "output_text",
                "text": "Hi",
                "annotations": []
            }]
        }
    });

    let error = translator
        .translate_event(&event)
        .expect_err("translation should fail");
    assert!(error
        .to_string()
        .contains("unsupported responses output role"));
}

#[test]
fn responses_stream_translator_rejects_unknown_output_item_types_on_completed_events() {
    let mut translator = StreamTranslator::new(
        UpstreamProtocol::Responses,
        UpstreamProtocol::ChatCompletions,
    )
    .expect("translator should exist");

    let event = json!({
        "type": "response.completed",
        "response_id": "resp-1",
        "response": {
            "id": "resp-1",
            "object": "response",
            "created_at": 1,
            "status": "completed",
            "model": "gpt-4.1-mini",
            "output": [
                {
                    "id": "unsupported_1",
                    "type": "unsupported_output"
                }
            ]
        }
    });

    let error = translator
        .translate_event(&event)
        .expect_err("translation should fail");
    assert!(error
        .to_string()
        .contains("unsupported responses output item type"));
}

#[test]
fn responses_stream_completed_item_errors_do_not_echo_payload_values() {
    let mut translator = StreamTranslator::new(
        UpstreamProtocol::Responses,
        UpstreamProtocol::ChatCompletions,
    )
    .expect("translator should exist");
    let error = translator
        .translate_event(&json!({
            "type": "response.completed",
            "response_id": "resp-malformed",
            "response": {
                "id": "resp-malformed",
                "model": "opaque",
                "output": ["SECRET_TOOL_OUTPUT"]
            }
        }))
        .expect_err("scalar output item should be rejected");
    assert_eq!(error.to_string(), "unsupported responses output item");
    assert!(!error.to_string().contains("SECRET_TOOL_OUTPUT"));
}

#[test]
fn responses_stream_translator_rejects_non_assistant_output_roles_on_completed_events() {
    let mut translator = StreamTranslator::new(
        UpstreamProtocol::Responses,
        UpstreamProtocol::ChatCompletions,
    )
    .expect("translator should exist");

    let event = json!({
        "type": "response.completed",
        "response_id": "resp-1",
        "response": {
            "id": "resp-1",
            "object": "response",
            "created_at": 1,
            "status": "completed",
            "model": "gpt-4.1-mini",
            "output": [
                {
                    "id": "msg-1",
                    "type": "message",
                    "status": "completed",
                    "role": "user",
                    "content": [{
                        "type": "output_text",
                        "text": "Hi",
                        "annotations": []
                    }]
                }
            ]
        }
    });

    let error = translator
        .translate_event(&event)
        .expect_err("translation should fail");
    assert!(error
        .to_string()
        .contains("unsupported responses output role"));
}

#[test]
fn responses_stream_translator_wraps_custom_input_and_preserves_call_id() {
    let mut translator = StreamTranslator::new(
        UpstreamProtocol::Responses,
        UpstreamProtocol::ChatCompletions,
    )
    .expect("translator should exist");

    let added = json!({
        "type": "response.output_item.added",
        "response_id": "resp-custom-stream",
        "output_index": 0,
        "item": {
            "id": "item-custom",
            "call_id": "call-custom",
            "type": "custom_tool_call",
            "status": "in_progress",
            "name": "apply_patch"
        }
    });
    let first_delta = json!({
        "type": "response.custom_tool_call_input.delta",
        "response_id": "resp-custom-stream",
        "output_index": 0,
        "item_id": "item-custom",
        "delta": "patch-"
    });
    let second_delta = json!({
        "type": "response.custom_tool_call_input.delta",
        "response_id": "resp-custom-stream",
        "output_index": 0,
        "item_id": "item-custom",
        "delta": "body"
    });
    let done = json!({
        "type": "response.custom_tool_call_input.done",
        "response_id": "resp-custom-stream",
        "output_index": 0,
        "item_id": "item-custom",
        "input": "patch-body"
    });

    let mut chunks = translator.translate_event(&added).unwrap();
    chunks.extend(translator.translate_event(&first_delta).unwrap());
    chunks.extend(translator.translate_event(&second_delta).unwrap());
    chunks.extend(translator.translate_event(&done).unwrap());

    let tool_chunks = chunks
        .iter()
        .filter(|chunk| chunk["choices"][0]["delta"]["tool_calls"].is_array())
        .collect::<Vec<_>>();
    assert!(!tool_chunks.is_empty());
    assert!(tool_chunks
        .iter()
        .all(|chunk| { chunk["choices"][0]["delta"]["tool_calls"][0]["id"] == "call-custom" }));
    let arguments = tool_chunks
        .iter()
        .filter_map(|chunk| {
            chunk["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"].as_str()
        })
        .collect::<String>();
    assert_eq!(arguments, "{\"input\":\"patch-body\"}");
}

#[test]
fn responses_stream_translator_uses_registry_for_tool_names() {
    let tools = json!([{"type":"namespace","name":"multi_agent_v1","tools":[
        {"type":"function","name":"spawn_agent","parameters":{"type":"object"}}
    ]}]);
    let adaptation = ToolAdapterRegistry::build(&tools, ToolTarget::FunctionsOnly).unwrap();
    let mapped_name = adaptation
        .registry
        .upstream_name(&ToolIdentity::namespace("multi_agent_v1", "spawn_agent"))
        .unwrap()
        .to_string();
    let mut translator = StreamTranslator::new_with_tool_registry(
        UpstreamProtocol::Responses,
        UpstreamProtocol::ChatCompletions,
        Some(adaptation.registry),
    )
    .expect("translator should exist");
    let chunks = translator
        .translate_event(&json!({
            "type": "response.output_item.added",
            "response_id": "resp-registry-stream",
            "output_index": 0,
            "item": {
                "id": "item-function",
                "call_id": "call-function",
                "type": "function_call",
                "status": "in_progress",
                "name": "spawn_agent",
                "namespace": "multi_agent_v1"
            }
        }))
        .unwrap();
    let tool_chunk = chunks
        .iter()
        .find(|chunk| chunk["choices"][0]["delta"]["tool_calls"].is_array())
        .unwrap();
    assert_eq!(
        tool_chunk["choices"][0]["delta"]["tool_calls"][0]["function"]["name"],
        mapped_name
    );
}

#[test]
fn responses_stream_custom_done_reconciles_a_partial_delta() {
    let mut translator = StreamTranslator::new(
        UpstreamProtocol::Responses,
        UpstreamProtocol::ChatCompletions,
    )
    .expect("translator should exist");

    let mut chunks = translator
        .translate_event(&json!({
            "type": "response.output_item.added",
            "response_id": "resp-custom-partial",
            "output_index": 0,
            "item": {
                "id": "item-custom",
                "call_id": "call-custom",
                "type": "custom_tool_call",
                "status": "in_progress",
                "name": "apply_patch"
            }
        }))
        .unwrap();
    chunks.extend(
        translator
            .translate_event(&json!({
                "type": "response.custom_tool_call_input.delta",
                "response_id": "resp-custom-partial",
                "output_index": 0,
                "item_id": "item-custom",
                "delta": "patch-"
            }))
            .unwrap(),
    );
    chunks.extend(
        translator
            .translate_event(&json!({
                "type": "response.custom_tool_call_input.done",
                "response_id": "resp-custom-partial",
                "output_index": 0,
                "item_id": "item-custom",
                "input": "patch-body"
            }))
            .unwrap(),
    );

    let arguments = chunks
        .iter()
        .filter_map(|chunk| {
            chunk["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"].as_str()
        })
        .collect::<String>();
    assert_eq!(arguments, "{\"input\":\"patch-body\"}");
}

#[test]
fn responses_stream_custom_input_appends_after_added_prefix_without_closing_json() {
    let mut translator = StreamTranslator::new(
        UpstreamProtocol::Responses,
        UpstreamProtocol::ChatCompletions,
    )
    .expect("translator should exist");

    let mut chunks = translator
        .translate_event(&json!({
            "type": "response.output_item.added",
            "response_id": "resp-custom-prefix",
            "output_index": 0,
            "item": {
                "id": "item-custom",
                "call_id": "call-custom",
                "type": "custom_tool_call",
                "status": "in_progress",
                "name": "apply_patch",
                "input": "patch-"
            }
        }))
        .unwrap();
    chunks.extend(
        translator
            .translate_event(&json!({
                "type": "response.custom_tool_call_input.delta",
                "response_id": "resp-custom-prefix",
                "output_index": 0,
                "item_id": "item-custom",
                "delta": "body"
            }))
            .unwrap(),
    );
    chunks.extend(
        translator
            .translate_event(&json!({
                "type": "response.custom_tool_call_input.done",
                "response_id": "resp-custom-prefix",
                "output_index": 0,
                "item_id": "item-custom",
                "input": "patch-body"
            }))
            .unwrap(),
    );

    let arguments = chunks
        .iter()
        .filter_map(|chunk| {
            chunk["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"].as_str()
        })
        .collect::<String>();
    assert_eq!(arguments, "{\"input\":\"patch-body\"}");
}
