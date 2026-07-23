use chat_responses_codex::protocol::tool_adapter::{ToolAdapterRegistry, ToolIdentity, ToolTarget};
use chat_responses_codex::protocol::{
    chat_request_to_responses_payload, chat_response_to_responses_payload,
    chat_response_to_responses_payload_with_context,
    chat_response_to_responses_payload_with_tool_registry, image_adapter,
    responses_request_to_chat_payload, responses_request_to_chat_payload_with_context,
    responses_response_to_chat_payload, ChatStreamCanonicalizer, ConversionContext,
    StreamAggregateResult, StreamResponseAggregator, StreamTranslator,
};
use chat_responses_codex::routing::UpstreamProtocol;
use serde_json::json;

#[test]
fn chat_request_converts_to_responses_payload() {
    let chat = json!({
        "model": "gpt-4.1-mini",
        "messages": [
            {"role": "system", "content": "You are terse."},
            {"role": "user", "content": "Hello"}
        ],
        "stream": false,
        "temperature": 0.2
    });

    let converted = chat_request_to_responses_payload(&chat).expect("conversion should work");

    assert_eq!(converted["model"], "gpt-4.1-mini");
    assert_eq!(converted["instructions"], "You are terse.");
    assert_eq!(converted["input"][0]["role"], "user");
    assert_eq!(converted["input"][0]["content"], "Hello");
}

#[test]
fn responses_request_converts_to_chat_payload() {
    let responses = json!({
        "model": "gpt-4.1-mini",
        "instructions": "You are terse.",
        "input": "Hello",
        "stream": false
    });

    let converted = responses_request_to_chat_payload(&responses).expect("conversion should work");

    assert_eq!(converted["model"], "gpt-4.1-mini");
    assert_eq!(converted["messages"][0]["role"], "system");
    assert_eq!(converted["messages"][0]["content"], "You are terse.");
    assert_eq!(converted["messages"][1]["role"], "user");
    assert_eq!(converted["messages"][1]["content"], "Hello");
}

#[test]
fn responses_chat_round_trip_preserves_url_detail_and_mixed_order() {
    let responses = json!({
        "model":"opaque",
        "input":[{
            "role":"user",
            "content":[
                {"type":"input_text","text":"before"},
                {"type":"input_image","image_url":"https://images.example/red.png","detail":"high"},
                {"type":"input_text","text":"after"}
            ]
        }]
    });

    let chat = responses_request_to_chat_payload(&responses).unwrap();
    assert_eq!(
        chat["messages"][0]["content"][1]["image_url"]["url"],
        "https://images.example/red.png"
    );
    assert_eq!(
        chat["messages"][0]["content"][1]["image_url"]["detail"],
        "high"
    );

    let round_trip = chat_request_to_responses_payload(&chat).unwrap();
    assert_eq!(
        round_trip["input"][0]["content"],
        responses["input"][0]["content"]
    );
}

#[test]
fn messages_base64_image_maps_to_mime_qualified_data_url_without_decode() {
    let block = json!({"type":"image","source":{
        "type":"base64","media_type":"image/png","data":"AAEC"}});
    let chat =
        image_adapter::messages_image_to_chat_part(&block, image_adapter::ImageDialect::all())
            .unwrap();
    assert_eq!(chat.value["image_url"]["url"], "data:image/png;base64,AAEC");
}

#[test]
fn messages_url_image_maps_to_chat_url_without_fetch() {
    let block = json!({"type":"image","source":{
        "type":"url","url":"https://images.example/shape.png"}});
    let chat =
        image_adapter::messages_image_to_chat_part(&block, image_adapter::ImageDialect::all())
            .unwrap();
    assert_eq!(
        chat.value["image_url"]["url"],
        "https://images.example/shape.png"
    );
}

#[test]
fn responses_request_converts_flat_tools_to_chat_payload() {
    let responses = json!({
        "model": "gpt-4.1-mini",
        "input": "Hello",
        "tools": [
            {
                "type": "function",
                "name": "get_weather",
                "description": "Get the weather",
                "parameters": {
                    "type": "object"
                }
            }
        ]
    });

    let converted = responses_request_to_chat_payload(&responses).expect("conversion should work");

    assert_eq!(converted["tools"][0]["type"], "function");
    assert_eq!(converted["tools"][0]["function"]["name"], "get_weather");
    assert_eq!(
        converted["tools"][0]["function"]["description"],
        "Get the weather"
    );
    assert_eq!(
        converted["tools"][0]["function"]["parameters"]["type"],
        "object"
    );
}

#[test]
fn responses_request_rejects_non_function_tools_for_chat_payload() {
    let responses = json!({
        "model": "gpt-4.1-mini",
        "input": "Hello",
        "tools": [
            {
                "type": "web_search"
            },
            {
                "type": "function",
                "name": "get_weather",
                "description": "Get the weather",
                "parameters": {
                    "type": "object"
                }
            }
        ],
        "tool_choice": {
            "type": "web_search"
        }
    });

    let error = responses_request_to_chat_payload(&responses).expect_err("conversion should fail");
    assert!(error
        .to_string()
        .contains("unsupported responses tool type"));
}

#[test]
fn responses_request_rejects_non_function_tool_choice_for_chat_payload() {
    let responses = json!({
        "model": "gpt-4.1-mini",
        "input": "Hello",
        "tools": [
            {
                "type": "function",
                "name": "get_weather",
                "description": "Get the weather",
                "parameters": {
                    "type": "object"
                }
            }
        ],
        "tool_choice": {
            "type": "web_search"
        }
    });

    let error = responses_request_to_chat_payload(&responses).expect_err("conversion should fail");
    assert!(error
        .to_string()
        .contains("unsupported responses tool_choice type"));
}

#[test]
fn responses_request_rejects_required_tool_choice_without_supported_tools() {
    let responses = json!({
        "model": "gpt-4.1-mini",
        "input": "Hello",
        "tool_choice": "required"
    });

    let error = responses_request_to_chat_payload(&responses).expect_err("conversion should fail");
    assert!(error
        .to_string()
        .contains("requires at least one supported function tool"));
}

#[test]
fn responses_request_drops_opaque_reasoning_history_for_chat_payload() {
    let responses = json!({
        "model": "gpt-4.1-mini",
        "input": [
            {
                "type": "reasoning",
                "id": "rs_opaque",
                "summary": []
            },
            {
                "role": "user",
                "content": "Continue without replaying private reasoning."
            }
        ]
    });

    let converted =
        responses_request_to_chat_payload(&responses).expect("conversion should reduce history");

    assert_eq!(converted["messages"].as_array().unwrap().len(), 1);
    assert_eq!(converted["messages"][0]["role"], "user");
    assert_eq!(
        converted["messages"][0]["content"],
        "Continue without replaying private reasoning."
    );
}

#[test]
fn chat_response_replays_namespace_function_calls_with_registry() {
    let tools = json!([
        {
            "type": "namespace",
            "name": "mcp__docs",
            "description": "Developer docs",
            "tools": [{
                "type": "function",
                "name": "search",
                "parameters": {"type": "object"}
            }]
        }
    ]);
    let adaptation = ToolAdapterRegistry::build(&tools, ToolTarget::FunctionsOnly).unwrap();
    let upstream_name = adaptation
        .registry
        .upstream_name(&ToolIdentity::namespace("mcp__docs", "search"))
        .unwrap()
        .to_string();
    let chat = json!({
        "id": "chatcmpl-tool",
        "object": "chat.completion",
        "created": 1,
        "model": "gpt-4.1-mini",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": upstream_name,
                        "arguments": "{\"q\":\"x\"}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }]
    });

    let responses =
        chat_response_to_responses_payload_with_tool_registry(&chat, Some(&adaptation.registry))
            .expect("conversion should work");

    assert_eq!(responses["output"][0]["type"], "function_call");
    assert_eq!(responses["output"][0]["call_id"], "call_1");
    assert_eq!(responses["output"][0]["name"], "search");
    assert_eq!(responses["output"][0]["namespace"], "mcp__docs");
    assert_eq!(responses["output"][0]["arguments"], "{\"q\":\"x\"}");
}

#[test]
fn chat_stream_replays_namespace_identity_in_every_terminal_responses_item() {
    let tools = json!([{
        "type": "namespace",
        "name": "mcp__docs",
        "description": "Developer docs",
        "tools": [{
            "type": "function",
            "name": "search",
            "parameters": {"type": "object"}
        }]
    }]);
    let adaptation = ToolAdapterRegistry::build(&tools, ToolTarget::FunctionsOnly).unwrap();
    let upstream_name = adaptation
        .registry
        .upstream_name(&ToolIdentity::namespace("mcp__docs", "search"))
        .unwrap()
        .to_string();
    let mut translator = StreamTranslator::new_with_tool_registry(
        UpstreamProtocol::ChatCompletions,
        UpstreamProtocol::Responses,
        Some(adaptation.registry),
    )
    .expect("translator should exist");

    let first = json!({
        "id": "chatcmpl-namespace",
        "created": 1,
        "model": "opaque",
        "choices": [{
            "index": 0,
            "delta": {"tool_calls": [{
                "index": 0,
                "id": "call_1",
                "type": "function",
                "function": {"name": upstream_name, "arguments": "{\"q\":"}
            }]},
            "finish_reason": null
        }]
    });
    let second = json!({
        "id": "chatcmpl-namespace",
        "created": 1,
        "model": "opaque",
        "choices": [{
            "index": 0,
            "delta": {"tool_calls": [{
                "index": 0,
                "function": {"arguments": "\"x\"}"}
            }]},
            "finish_reason": "tool_calls"
        }]
    });

    let mut events = translator.translate_event(&first).unwrap();
    events.extend(translator.translate_event(&second).unwrap());
    events.extend(translator.finish().unwrap());

    let added = events
        .iter()
        .find(|event| event["type"] == "response.output_item.added")
        .expect("function call added event");
    let arguments_done = events
        .iter()
        .find(|event| event["type"] == "response.function_call_arguments.done")
        .expect("function arguments done event");
    let item_done = events
        .iter()
        .find(|event| event["type"] == "response.output_item.done")
        .expect("function call item done event");
    let completed = events
        .iter()
        .find(|event| event["type"] == "response.completed")
        .expect("response completed event");

    assert_eq!(added["item"]["name"], "search");
    assert_eq!(added["item"]["namespace"], "mcp__docs");
    assert_eq!(arguments_done["name"], "search");
    assert_eq!(item_done["item"]["name"], "search");
    assert_eq!(item_done["item"]["namespace"], "mcp__docs");
    assert_eq!(completed["response"]["output"][0]["name"], "search");
    assert_eq!(completed["response"]["output"][0]["namespace"], "mcp__docs");
}

#[test]
fn responses_request_converts_developer_message_to_system_role() {
    let responses = json!({
        "model": "gpt-4.1-mini",
        "input": [
            {
                "role": "developer",
                "content": "Use JSON."
            },
            {
                "role": "user",
                "content": "Hello"
            }
        ]
    });

    let converted = responses_request_to_chat_payload(&responses).expect("conversion should work");

    assert_eq!(converted["messages"][0]["role"], "system");
    assert_eq!(converted["messages"][0]["content"], "Use JSON.");
    assert_eq!(converted["messages"][1]["role"], "user");
    assert_eq!(converted["messages"][1]["content"], "Hello");
}

#[test]
fn chat_request_converts_common_tool_call_fields_to_responses_payload() {
    let chat = json!({
        "model": "gpt-4.1-mini",
        "messages": [
            {"role": "system", "content": "You are terse."},
            {"role": "developer", "content": "Use JSON."},
            {"role": "user", "content": "Hello"},
            {
                "role": "assistant",
                "tool_calls": [
                    {
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"city\":\"Paris\"}"
                        }
                    }
                ]
            },
            {
                "role": "tool",
                "tool_call_id": "call_1",
                "content": "Sunny"
            }
        ],
        "tools": [
            {
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "Get the weather",
                    "parameters": {
                        "type": "object"
                    }
                }
            }
        ],
        "tool_choice": "auto",
        "parallel_tool_calls": true,
        "top_p": 0.9,
        "stop": ["\n"],
        "metadata": {
            "trace_id": "abc"
        },
        "max_tokens": 128
    });

    let converted = chat_request_to_responses_payload(&chat).expect("conversion should work");

    assert_eq!(converted["instructions"], "You are terse.\nUse JSON.");
    assert_eq!(converted["max_output_tokens"], 128);
    assert_eq!(converted["top_p"], 0.9);
    assert_eq!(converted["stop"], json!(["\n"]));
    assert_eq!(converted["tool_choice"], "auto");
    assert_eq!(converted["parallel_tool_calls"], true);
    assert_eq!(converted["metadata"]["trace_id"], "abc");
    assert_eq!(converted["tools"][0]["type"], "function");
    assert_eq!(converted["tools"][0]["function"]["name"], "get_weather");
    assert_eq!(converted["input"][0]["role"], "user");
    assert_eq!(converted["input"][0]["content"], "Hello");
    assert_eq!(converted["input"][1]["type"], "function_call");
    assert_eq!(converted["input"][1]["call_id"], "call_1");
    assert_eq!(converted["input"][1]["name"], "get_weather");
    assert_eq!(converted["input"][1]["arguments"], "{\"city\":\"Paris\"}");
    assert_eq!(converted["input"][2]["type"], "function_call_output");
    assert_eq!(converted["input"][2]["call_id"], "call_1");
    assert_eq!(converted["input"][2]["output"], "Sunny");
}

#[test]
fn chat_request_converts_flat_tool_call_fields_to_responses_payload() {
    let chat = json!({
        "model": "gpt-4.1-mini",
        "messages": [
            {
                "role": "assistant",
                "tool_calls": [
                    {
                        "id": "call_1",
                        "name": "get_weather",
                        "arguments": "{\"city\":\"Paris\"}"
                    }
                ]
            }
        ]
    });

    let converted = chat_request_to_responses_payload(&chat).expect("conversion should work");

    assert_eq!(converted["input"][0]["type"], "function_call");
    assert_eq!(converted["input"][0]["call_id"], "call_1");
    assert_eq!(converted["input"][0]["name"], "get_weather");
    assert_eq!(converted["input"][0]["arguments"], "{\"city\":\"Paris\"}");
}

#[test]
fn chat_request_converts_shared_openai_fields_to_responses_payload() {
    let chat = json!({
        "model": "gpt-4.1-mini",
        "messages": [
            {"role": "user", "content": "Hello"}
        ],
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "reply",
                "schema": {
                    "type": "object"
                }
            }
        },
        "service_tier": "priority",
        "store": true,
        "safety_identifier": "user-123",
        "prompt_cache_key": "cache-123",
        "prompt_cache_retention": "24h",
        "verbosity": "high",
        "stream_options": {
            "include_obfuscation": false,
            "include_usage": true
        }
    });

    let converted = chat_request_to_responses_payload(&chat).expect("conversion should work");

    assert_eq!(converted["service_tier"], "priority");
    assert_eq!(converted["store"], true);
    assert_eq!(converted["safety_identifier"], "user-123");
    assert_eq!(converted["prompt_cache_key"], "cache-123");
    assert_eq!(converted["prompt_cache_retention"], "24h");
    assert_eq!(converted["text"]["format"]["type"], "json_schema");
    assert_eq!(converted["text"]["format"]["json_schema"]["name"], "reply");
    assert_eq!(converted["text"]["verbosity"], "high");
    assert_eq!(converted["stream_options"]["include_obfuscation"], false);
    assert!(converted["stream_options"].get("include_usage").is_none());
}

#[test]
fn chat_request_rejects_multiple_choices_for_responses_payload() {
    let chat = json!({
        "model": "gpt-4.1-mini",
        "messages": [
            {"role": "user", "content": "Hello"}
        ],
        "n": 2
    });

    let error = chat_request_to_responses_payload(&chat).expect_err("conversion should fail");
    assert!(error
        .to_string()
        .contains("multiple chat completion choices are not supported"));
}

#[test]
fn responses_request_converts_tool_calls_and_outputs_to_chat_payload() {
    let responses = json!({
        "model": "gpt-4.1-mini",
        "instructions": "You are terse.",
        "input": [
            {"role": "user", "content": "Hello"},
            {
                "type": "function_call",
                "call_id": "call_1",
                "name": "get_weather",
                "arguments": "{\"city\":\"Paris\"}"
            },
            {
                "type": "function_call_output",
                "call_id": "call_1",
                "output": "Sunny"
            },
            {
                "type": "message",
                "role": "assistant",
                "content": [
                    {
                        "type": "output_text",
                        "text": "Use the weather"
                    }
                ]
            }
        ],
        "max_output_tokens": 128,
        "top_p": 0.9,
        "stop": ["\n"],
        "metadata": {
            "trace_id": "abc"
        },
        "tools": [
            {
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "Get the weather",
                    "parameters": {
                        "type": "object"
                    }
                }
            }
        ],
        "tool_choice": "auto",
        "parallel_tool_calls": true
    });

    let converted = responses_request_to_chat_payload(&responses).expect("conversion should work");

    assert_eq!(converted["model"], "gpt-4.1-mini");
    assert_eq!(converted["messages"][0]["role"], "system");
    assert_eq!(converted["messages"][0]["content"], "You are terse.");
    assert_eq!(converted["messages"][1]["role"], "user");
    assert_eq!(converted["messages"][1]["content"], "Hello");
    assert_eq!(converted["messages"][2]["role"], "assistant");
    assert_eq!(converted["messages"][2]["tool_calls"][0]["id"], "call_1");
    assert_eq!(
        converted["messages"][2]["tool_calls"][0]["function"]["name"],
        "get_weather"
    );
    assert_eq!(
        converted["messages"][2]["tool_calls"][0]["function"]["arguments"],
        "{\"city\":\"Paris\"}"
    );
    assert_eq!(converted["messages"][3]["role"], "tool");
    assert_eq!(converted["messages"][3]["tool_call_id"], "call_1");
    assert_eq!(converted["messages"][3]["content"], "Sunny");
    assert_eq!(converted["messages"][4]["role"], "assistant");
    assert_eq!(converted["messages"][4]["content"], "Use the weather");
    assert_eq!(converted["max_tokens"], 128);
    assert_eq!(converted["top_p"], 0.9);
    assert_eq!(converted["stop"], json!(["\n"]));
    assert_eq!(converted["tool_choice"], "auto");
    assert_eq!(converted["parallel_tool_calls"], true);
    assert_eq!(converted["metadata"]["trace_id"], "abc");
    assert_eq!(converted["tools"][0]["function"]["name"], "get_weather");
}

#[test]
fn responses_request_converts_shared_openai_fields_to_chat_payload() {
    let responses = json!({
        "model": "gpt-4.1-mini",
        "input": "Hello",
        "text": {
            "format": {
                "type": "json_schema",
                "json_schema": {
                    "name": "reply",
                    "schema": {
                        "type": "object"
                    }
                }
            },
            "verbosity": "high"
        },
        "service_tier": "priority",
        "store": true,
        "safety_identifier": "user-123",
        "prompt_cache_key": "cache-123",
        "prompt_cache_retention": "24h",
        "stream_options": {
            "include_obfuscation": false
        }
    });

    let converted = responses_request_to_chat_payload(&responses).expect("conversion should work");

    assert_eq!(converted["service_tier"], "priority");
    assert_eq!(converted["store"], true);
    assert_eq!(converted["safety_identifier"], "user-123");
    assert_eq!(converted["prompt_cache_key"], "cache-123");
    assert_eq!(converted["prompt_cache_retention"], "24h");
    assert_eq!(converted["response_format"]["type"], "json_schema");
    assert_eq!(converted["response_format"]["json_schema"]["name"], "reply");
    assert_eq!(converted["verbosity"], "high");
    assert_eq!(converted["stream_options"]["include_obfuscation"], false);
}

#[test]
fn responses_request_merges_function_call_with_following_assistant_message_before_tool_output() {
    let responses = json!({
        "model": "gpt-4.1-mini",
        "input": [
            {
                "type": "function_call",
                "call_id": "exec_command:1",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"pwd\"}"
            },
            {
                "type": "message",
                "role": "assistant",
                "content": [
                    {
                        "type": "output_text",
                        "text": "Running command"
                    }
                ]
            },
            {
                "type": "function_call_output",
                "call_id": "exec_command:1",
                "output": "/home/kavin"
            },
            {
                "role": "user",
                "content": "Continue"
            }
        ]
    });

    let converted = responses_request_to_chat_payload(&responses).expect("conversion should work");
    let messages = converted["messages"].as_array().expect("messages array");

    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0]["role"], "assistant");
    assert_eq!(messages[0]["content"], "Running command");
    assert_eq!(messages[0]["tool_calls"][0]["id"], "exec_command:1");
    assert_eq!(
        messages[0]["tool_calls"][0]["function"]["arguments"],
        "{\"cmd\":\"pwd\"}"
    );
    assert_eq!(messages[1]["role"], "tool");
    assert_eq!(messages[1]["tool_call_id"], "exec_command:1");
    assert_eq!(messages[1]["content"], "/home/kavin");
    assert_eq!(messages[2]["role"], "user");
    assert_eq!(messages[2]["content"], "Continue");
}

#[test]
fn responses_request_merges_consecutive_function_calls_into_one_assistant_message() {
    let responses = json!({
        "model": "gpt-4.1-mini",
        "input": [
            {
                "type": "function_call",
                "call_id": "exec_command:1",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"pwd\"}"
            },
            {
                "type": "function_call",
                "call_id": "write_stdin:1",
                "name": "write_stdin",
                "arguments": "{\"chars\":\"help\"}"
            },
            {
                "type": "function_call_output",
                "call_id": "exec_command:1",
                "output": "/home/kavin"
            },
            {
                "type": "function_call_output",
                "call_id": "write_stdin:1",
                "output": "usage"
            }
        ]
    });

    let converted = responses_request_to_chat_payload(&responses).expect("conversion should work");
    let messages = converted["messages"].as_array().expect("messages array");

    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0]["role"], "assistant");
    assert_eq!(messages[0]["tool_calls"].as_array().map(Vec::len), Some(2));
    assert_eq!(messages[0]["tool_calls"][0]["id"], "exec_command:1");
    assert_eq!(messages[0]["tool_calls"][1]["id"], "write_stdin:1");
    assert_eq!(messages[1]["role"], "tool");
    assert_eq!(messages[1]["tool_call_id"], "exec_command:1");
    assert_eq!(messages[2]["role"], "tool");
    assert_eq!(messages[2]["tool_call_id"], "write_stdin:1");
}

#[test]
fn chat_response_converts_tool_calls_to_responses_output() {
    let chat_response = json!({
        "id": "chatcmpl-1",
        "object": "chat.completion",
        "created": 1,
        "model": "gpt-4.1-mini",
        "choices": [
            {
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [
                        {
                            "id": "call_1",
                            "type": "function",
                            "function": {
                                "name": "get_weather",
                                "arguments": "{\"city\":\"Paris\"}"
                            }
                        }
                    ]
                },
                "finish_reason": "tool_calls"
            }
        ],
        "usage": {
            "prompt_tokens": 1,
            "completion_tokens": 0,
            "total_tokens": 1,
            "prompt_tokens_details": {
                "cached_tokens": 1
            },
            "completion_tokens_details": {
                "reasoning_tokens": 0
            }
        }
    });

    let converted =
        chat_response_to_responses_payload(&chat_response).expect("conversion should work");

    assert_eq!(converted["id"], "chatcmpl-1");
    assert_eq!(converted["object"], "response");
    assert_eq!(converted["status"], "completed");
    assert_eq!(converted["output"][0]["type"], "function_call");
    assert!(converted["output"][0]["id"].is_string());
    assert_eq!(converted["output"][0]["status"], "completed");
    assert_eq!(converted["output"][0]["call_id"], "call_1");
    assert_eq!(converted["output"][0]["name"], "get_weather");
    assert_eq!(converted["output"][0]["arguments"], "{\"city\":\"Paris\"}");
    assert_eq!(converted["usage"]["input_tokens"], 1);
    assert_eq!(converted["usage"]["output_tokens"], 0);
    assert_eq!(converted["usage"]["total_tokens"], 1);
    assert!(converted["usage"].get("prompt_tokens").is_none());
    assert!(converted["usage"].get("completion_tokens").is_none());
    assert_eq!(
        converted["usage"]["input_tokens_details"]["cached_tokens"],
        1
    );
    assert_eq!(
        converted["usage"]["output_tokens_details"]["reasoning_tokens"],
        0
    );
}

#[test]
fn chat_response_defaults_required_responses_usage_details() {
    let chat_response = json!({
        "id": "chatcmpl-usage-defaults",
        "object": "chat.completion",
        "created": 1,
        "model": "opaque-model",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "Complete"},
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 4,
            "completion_tokens": 2,
            "total_tokens": 6
        }
    });

    let converted =
        chat_response_to_responses_payload(&chat_response).expect("conversion should work");

    assert_eq!(
        converted["usage"]["input_tokens_details"]["cached_tokens"],
        0
    );
    assert_eq!(
        converted["usage"]["output_tokens_details"]["reasoning_tokens"],
        0
    );
}

#[test]
fn chat_response_rejects_multiple_choices_for_responses_payload() {
    let chat_response = json!({
        "id": "chatcmpl-1",
        "object": "chat.completion",
        "created": 1,
        "model": "gpt-4.1-mini",
        "choices": [
            {
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Hi"
                },
                "finish_reason": "stop"
            },
            {
                "index": 1,
                "message": {
                    "role": "assistant",
                    "content": "Bye"
                },
                "finish_reason": "stop"
            }
        ]
    });

    let error =
        chat_response_to_responses_payload(&chat_response).expect_err("conversion should fail");
    assert!(error
        .to_string()
        .contains("multiple chat completion choices are not supported"));
}

#[test]
fn responses_response_converts_tool_calls_to_chat_payload() {
    let responses = json!({
        "id": "resp-1",
        "object": "response",
        "created": 1,
        "model": "gpt-4.1-mini",
        "output": [
            {
                "id": "fc_1",
                "call_id": "call_1",
                "type": "function_call",
                "name": "get_weather",
                "arguments": "{\"city\":\"Paris\"}"
            }
        ],
        "usage": {
            "input_tokens": 1,
            "output_tokens": 0,
            "total_tokens": 1,
            "input_tokens_details": {
                "cached_tokens": 1
            },
            "output_tokens_details": {
                "reasoning_tokens": 0
            }
        }
    });

    let converted = responses_response_to_chat_payload(&responses).expect("conversion should work");

    assert_eq!(converted["id"], "resp-1");
    assert_eq!(converted["object"], "chat.completion");
    assert_eq!(converted["choices"][0]["message"]["role"], "assistant");
    assert_eq!(converted["choices"][0]["message"]["content"], json!(null));
    assert_eq!(
        converted["choices"][0]["message"]["tool_calls"][0]["id"],
        "call_1"
    );
    assert_eq!(
        converted["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
        "get_weather"
    );
    assert_eq!(converted["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(converted["usage"]["prompt_tokens"], 1);
    assert_eq!(converted["usage"]["completion_tokens"], 0);
    assert_eq!(converted["usage"]["total_tokens"], 1);
    assert!(converted["usage"].get("input_tokens").is_none());
    assert!(converted["usage"].get("output_tokens").is_none());
    assert_eq!(
        converted["usage"]["prompt_tokens_details"]["cached_tokens"],
        1
    );
    assert_eq!(
        converted["usage"]["completion_tokens_details"]["reasoning_tokens"],
        0
    );
}

#[test]
fn responses_response_rejects_multiple_assistant_messages_for_chat_payload() {
    let responses = json!({
        "id": "resp-1",
        "object": "response",
        "created": 1,
        "model": "gpt-4.1-mini",
        "output": [
            {
                "id": "msg_1",
                "type": "message",
                "role": "assistant",
                "content": [
                    {
                        "type": "output_text",
                        "text": "Hi",
                        "annotations": []
                    }
                ]
            },
            {
                "id": "msg_2",
                "type": "message",
                "role": "assistant",
                "content": [
                    {
                        "type": "output_text",
                        "text": "Bye",
                        "annotations": []
                    }
                ]
            }
        ]
    });

    let error = responses_response_to_chat_payload(&responses).expect_err("conversion should fail");
    assert!(error
        .to_string()
        .contains("multiple assistant messages are not supported"));
}

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
fn chat_reasoning_content_becomes_responses_reasoning_before_function_call() {
    let chat = json!({
        "id":"chatcmpl-reasoning","model":"opaque","choices":[{"index":0,
        "message":{"role":"assistant","content":null,"reasoning_content":"exact-thought",
        "tool_calls":[{"id":"call_7","type":"function","function":{
            "name":"lookup","arguments":"{\"key\":\"value\"}"}}]},
        "finish_reason":"tool_calls"}]
    });
    let response = chat_response_to_responses_payload_with_context(
        &chat,
        &ConversionContext::reasoning_content(),
    )
    .unwrap();
    assert_eq!(response["output"][0]["type"], "reasoning");
    assert_eq!(
        response["output"][0]["content"][0]["type"],
        "reasoning_text"
    );
    assert_eq!(response["output"][0]["content"][0]["text"], "exact-thought");
    assert_eq!(response["output"][1]["type"], "function_call");
}

#[test]
fn responses_reasoning_and_call_replay_merge_into_one_chat_assistant_message() {
    let responses = json!({"model":"opaque","input":[
        {"type":"reasoning","id":"rs_7","content":[{"type":"reasoning_text","text":"exact-thought"}]},
        {"type":"function_call","call_id":"call_7","name":"lookup","arguments":"{\"key\":\"value\"}"},
        {"type":"function_call_output","call_id":"call_7","output":"result"}
    ]});
    let chat = responses_request_to_chat_payload_with_context(
        &responses,
        &ConversionContext::reasoning_content(),
    )
    .unwrap();
    assert_eq!(chat["messages"][0]["reasoning_content"], "exact-thought");
    assert_eq!(chat["messages"][0]["tool_calls"][0]["id"], "call_7");
    assert_eq!(chat["messages"][1]["tool_call_id"], "call_7");
}

#[test]
fn chat_stream_include_usage_tail_completes_with_usage() {
    let mut translator = StreamTranslator::new(
        UpstreamProtocol::ChatCompletions,
        UpstreamProtocol::Responses,
    )
    .expect("translator should exist");

    let content = json!({
        "id": "chatcmpl-usage-tail",
        "created": 1,
        "model": "opaque",
        "choices": [{
            "index": 0,
            "delta": {"content": "OK"},
            "finish_reason": null
        }],
        "usage": null
    });
    translator.translate_event(&content).unwrap();

    let finish_reason = json!({
        "id": "chatcmpl-usage-tail",
        "created": 1,
        "model": "opaque",
        "choices": [{
            "index": 0,
            "delta": {},
            "finish_reason": "stop"
        }],
        "usage": null
    });
    let finish_reason_events = translator.translate_event(&finish_reason).unwrap();
    assert!(
        !finish_reason_events.iter().any(|event| {
            matches!(
                event["type"].as_str(),
                Some("response.completed" | "response.incomplete")
            )
        }),
        "finish_reason must not precede a possible include_usage tail chunk"
    );

    let usage_tail = json!({
        "id": "chatcmpl-usage-tail",
        "created": 1,
        "model": "opaque",
        "choices": [],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 2,
            "total_tokens": 12
        }
    });
    let tail_events = translator.translate_event(&usage_tail).unwrap();
    let completed = tail_events
        .iter()
        .find(|event| event["type"] == "response.completed")
        .expect("usage tail should emit response.completed");
    assert_eq!(completed["response"]["usage"]["input_tokens"], 10);
    assert_eq!(completed["response"]["usage"]["output_tokens"], 2);
    assert_eq!(completed["response"]["usage"]["total_tokens"], 12);
}

#[test]
fn chat_stream_ignores_events_after_terminal_output() {
    let mut translator = StreamTranslator::new(
        UpstreamProtocol::ChatCompletions,
        UpstreamProtocol::Responses,
    )
    .expect("translator should exist");

    let content = json!({
        "id": "chatcmpl-terminal",
        "created": 1,
        "model": "opaque",
        "choices": [{
            "index": 0,
            "delta": {"content": "first"},
            "finish_reason": null
        }]
    });
    translator.translate_event(&content).unwrap();
    let terminal = translator.finish().unwrap();
    assert!(terminal
        .iter()
        .any(|event| event["type"] == "response.completed"));

    let duplicate = json!({
        "id": "chatcmpl-terminal",
        "created": 1,
        "model": "opaque",
        "choices": [{
            "index": 0,
            "delta": {"content": "duplicate"},
            "finish_reason": "stop"
        }]
    });
    assert!(translator.translate_event(&duplicate).unwrap().is_empty());
    assert!(translator.finish().unwrap().is_empty());
}

#[test]
fn chat_stream_canonicalizer_synthesizes_stop_at_eof_after_text_output() {
    let mut canonicalizer = ChatStreamCanonicalizer::new("chatcmpl-stable", "opaque", 1);
    canonicalizer
        .push(json!({
            "choices": [{
                "delta": {"content": "partial"},
                "finish_reason": null
            }]
        }))
        .unwrap();

    let terminal = canonicalizer.finish().unwrap();
    assert_eq!(terminal.len(), 1);
    assert_eq!(terminal[0]["id"], "chatcmpl-stable");
    assert_eq!(terminal[0]["model"], "opaque");
    assert_eq!(terminal[0]["created"], 1);
    assert_eq!(terminal[0]["choices"][0]["delta"], json!({}));
    assert_eq!(terminal[0]["choices"][0]["finish_reason"], "stop");
}

#[test]
fn chat_stream_canonicalizer_normalizes_null_delta_to_empty_object() {
    let mut canonicalizer = ChatStreamCanonicalizer::new("id", "model", 1);
    let events = canonicalizer
        .push(json!({
            "choices": [{"index": 0, "delta": null, "finish_reason": null}]
        }))
        .unwrap();

    assert_eq!(events[0]["choices"][0]["delta"], json!({}));
    assert!(canonicalizer.finish().is_err());
}

#[test]
fn chat_stream_canonicalizer_stabilizes_sparse_identity_and_terminal_alias() {
    let mut canonicalizer = ChatStreamCanonicalizer::new("chatcmpl-request", "glm-5.2", 42);
    let first = canonicalizer
        .push(json!({
            "choices": [{"delta": {"content": "hello"}}],
            "usage": {"prompt_tokens": 2, "completion_tokens": 1, "total_tokens": 3}
        }))
        .unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0]["id"], "chatcmpl-request");
    assert_eq!(first[0]["object"], "chat.completion.chunk");
    assert_eq!(first[0]["created"], 42);
    assert_eq!(first[0]["model"], "glm-5.2");
    assert_eq!(first[0]["choices"][0]["index"], 0);
    assert!(first[0].get("usage").is_none());

    let terminal = canonicalizer
        .push(json!({
            "choices": [{"delta": {}, "finish_reason": "end_turn"}],
            "usage": {"prompt_tokens": 2, "completion_tokens": 2, "total_tokens": 4}
        }))
        .unwrap();
    assert_eq!(terminal[0]["choices"][0]["finish_reason"], "stop");
    assert_eq!(
        canonicalizer.finish_after_done().unwrap()[0]["usage"]["total_tokens"],
        4
    );
}

#[test]
fn chat_stream_canonicalizer_keeps_first_identity_when_later_chunks_drift() {
    let mut canonicalizer = ChatStreamCanonicalizer::new("fallback", "fallback", 1);
    canonicalizer
        .push(json!({
            "id": "first-id",
            "model": "first-model",
            "created": 10,
            "choices": [{"delta": {"content": "hello"}, "finish_reason": null}]
        }))
        .unwrap();

    let terminal = canonicalizer
        .push(json!({
            "id": "later-id",
            "model": "later-model",
            "created": 20,
            "choices": [{"delta": {}, "finish_reason": "stop"}]
        }))
        .unwrap();

    assert_eq!(terminal[0]["id"], "first-id");
    assert_eq!(terminal[0]["model"], "first-model");
    assert_eq!(terminal[0]["created"], 10);
}

#[test]
fn chat_stream_canonicalizer_rejects_unknown_terminal() {
    let mut unknown = ChatStreamCanonicalizer::new("id", "model", 1);
    assert!(unknown
        .push(json!({
            "choices": [{"delta": {"content": "x"}, "finish_reason": "provider_done"}]
        }))
        .is_err());
}

#[test]
fn chat_stream_canonicalizer_normalizes_minimax_tool_stop_terminal() {
    let mut canonicalizer = ChatStreamCanonicalizer::new("id", "MiniMax-M2.7", 1);
    canonicalizer
        .push(json!({
            "choices": [{
                "index": 0,
                "delta": {"reasoning_content": "I should use the tool"},
                "finish_reason": null
            }]
        }))
        .unwrap();

    let terminal = canonicalizer
        .push(json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "exec_command", "arguments": "{\"cmd\":\"pwd\"}"}
                    }]
                },
                "finish_reason": "stop"
            }]
        }))
        .unwrap();

    assert_eq!(terminal[0]["choices"][0]["finish_reason"], "tool_calls");
    assert!(canonicalizer.finish_after_done().unwrap().is_empty());
}

#[test]
fn chat_stream_canonicalizer_tracks_terminal_semantics_per_choice() {
    let mut canonicalizer = ChatStreamCanonicalizer::new("id", "MiniMax-M2.7", 1);
    let terminal = canonicalizer
        .push(json!({
            "choices": [
                {
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": "call_1",
                            "type": "function",
                            "function": {"name": "exec_command", "arguments": "{}"}
                        }]
                    },
                    "finish_reason": "stop"
                },
                {
                    "index": 1,
                    "delta": {"content": "done"},
                    "finish_reason": "stop"
                }
            ]
        }))
        .unwrap();

    assert_eq!(terminal[0]["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(terminal[0]["choices"][1]["finish_reason"], "stop");
    assert!(canonicalizer.finish_after_done().unwrap().is_empty());
}

#[test]
fn chat_stream_canonicalizer_completes_each_unterminated_choice_after_done() {
    let mut canonicalizer = ChatStreamCanonicalizer::new("id", "model", 1);
    canonicalizer
        .push(json!({
            "choices": [
                {"index": 0, "delta": {"content": "first"}, "finish_reason": null},
                {"index": 1, "delta": {"content": "second"}, "finish_reason": null}
            ]
        }))
        .unwrap();
    canonicalizer
        .push(json!({
            "choices": [
                {"index": 0, "delta": {}, "finish_reason": "stop"}
            ]
        }))
        .unwrap();

    let completed = canonicalizer.finish_after_done().unwrap();
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0]["choices"].as_array().unwrap().len(), 1);
    assert_eq!(completed[0]["choices"][0]["index"], 1);
    assert_eq!(completed[0]["choices"][0]["finish_reason"], "stop");
}

#[test]
fn chat_stream_canonicalizer_synthesizes_tool_calls_at_eof_after_tool_output() {
    let mut canonicalizer = ChatStreamCanonicalizer::new("id", "model", 1);
    canonicalizer
        .push(json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "read_file", "arguments": "{}"}
                    }]
                },
                "finish_reason": null
            }]
        }))
        .unwrap();

    let terminal = canonicalizer.finish().unwrap();
    assert_eq!(terminal[0]["choices"][0]["delta"], json!({}));
    assert_eq!(terminal[0]["choices"][0]["finish_reason"], "tool_calls");
}

#[test]
fn chat_stream_canonicalizer_rejects_eof_with_only_role_and_usage() {
    let mut canonicalizer = ChatStreamCanonicalizer::new("id", "model", 1);
    canonicalizer
        .push(json!({
            "choices": [{
                "index": 0,
                "delta": {"role": "assistant"},
                "finish_reason": null
            }]
        }))
        .unwrap();
    canonicalizer
        .push(json!({
            "choices": [],
            "usage": {"prompt_tokens": 1, "completion_tokens": 0, "total_tokens": 1}
        }))
        .unwrap();

    assert!(canonicalizer.latest_usage().is_some());
    assert!(canonicalizer.finish().is_err());
}

#[test]
fn chat_stream_canonicalizer_rejects_eof_when_any_choice_lacks_terminal() {
    let mut canonicalizer = ChatStreamCanonicalizer::new("id", "model", 1);
    canonicalizer
        .push(json!({
            "choices": [
                {"index": 0, "delta": {"content": "first"}, "finish_reason": "stop"},
                {"index": 1, "delta": {"role": "assistant"}, "finish_reason": null}
            ]
        }))
        .unwrap();

    assert!(canonicalizer.finish().is_err());
}

#[test]
fn chat_stream_canonicalizer_removes_null_tool_extensions() {
    let mut canonicalizer = ChatStreamCanonicalizer::new("fallback", "fallback", 1);
    let events = canonicalizer
        .push(json!({
            "id": "chatcmpl-provider",
            "created": 2,
            "model": "opaque",
            "choices": [{
                "index": 0,
                "delta": {
                    "role": "assistant",
                    "content": "OK",
                    "tool_calls": null,
                    "function_call": null,
                    "function_calls": null
                },
                "finish_reason": "stop"
            }]
        }))
        .unwrap();
    let delta = events[0]["choices"][0]["delta"].as_object().unwrap();
    assert!(!delta.contains_key("tool_calls"));
    assert!(!delta.contains_key("function_call"));
    assert!(!delta.contains_key("function_calls"));
}

#[test]
fn chat_stream_closes_reasoning_before_starting_message_item() {
    let mut translator = StreamTranslator::new(
        UpstreamProtocol::ChatCompletions,
        UpstreamProtocol::Responses,
    )
    .expect("translator should exist");

    let reasoning = json!({
        "id": "chatcmpl-lifecycle",
        "created": 1,
        "model": "opaque",
        "choices": [{
            "index": 0,
            "delta": {"reasoning_content": "plan"},
            "finish_reason": null
        }]
    });
    let reasoning_events = translator.translate_event(&reasoning).unwrap();
    assert_eq!(reasoning_events[1]["output_index"], 0);

    let message = json!({
        "id": "chatcmpl-lifecycle",
        "created": 1,
        "model": "opaque",
        "choices": [{
            "index": 0,
            "delta": {"content": "answer"},
            "finish_reason": null
        }]
    });
    let message_events = translator.translate_event(&message).unwrap();
    let reasoning_done = message_events
        .iter()
        .position(|event| event["type"] == "response.output_item.done")
        .expect("reasoning item must be closed before the message starts");
    let message_added = message_events
        .iter()
        .position(|event| event["type"] == "response.output_item.added")
        .expect("message item should be added");
    assert!(reasoning_done < message_added);
    assert_eq!(message_events[reasoning_done]["output_index"], 0);
    assert_eq!(
        message_events[reasoning_done]["item"]["status"],
        "completed"
    );
    assert_eq!(message_events[message_added]["output_index"], 1);
    assert!(message_events[message_added + 1]["type"] == "response.output_text.delta");
    assert_eq!(message_events[message_added + 1]["output_index"], 1);
}

#[test]
fn chat_stream_closes_reasoning_before_starting_tool_item() {
    let mut translator = StreamTranslator::new(
        UpstreamProtocol::ChatCompletions,
        UpstreamProtocol::Responses,
    )
    .expect("translator should exist");

    let reasoning = json!({
        "id": "chatcmpl-tool-lifecycle",
        "created": 1,
        "model": "opaque",
        "choices": [{
            "index": 0,
            "delta": {"reasoning_content": "plan"},
            "finish_reason": null
        }]
    });
    translator.translate_event(&reasoning).unwrap();

    let tool = json!({
        "id": "chatcmpl-tool-lifecycle",
        "created": 1,
        "model": "opaque",
        "choices": [{
            "index": 0,
            "delta": {"tool_calls": [{
                "index": 0,
                "id": "call_1",
                "type": "function",
                "function": {"name": "lookup", "arguments": "{}"}
            }]},
            "finish_reason": null
        }]
    });
    let tool_events = translator.translate_event(&tool).unwrap();
    let reasoning_done = tool_events
        .iter()
        .position(|event| event["type"] == "response.output_item.done")
        .expect("reasoning item must be closed before the tool starts");
    let tool_added = tool_events
        .iter()
        .position(|event| event["type"] == "response.output_item.added")
        .expect("tool item should be added");
    assert!(reasoning_done < tool_added);
    assert_eq!(tool_events[reasoning_done]["output_index"], 0);
    assert_eq!(tool_events[tool_added]["output_index"], 1);
}

#[test]
fn chat_stream_length_keeps_closed_reasoning_completed() {
    let mut translator = StreamTranslator::new(
        UpstreamProtocol::ChatCompletions,
        UpstreamProtocol::Responses,
    )
    .expect("translator should exist");
    let chunks = [
        json!({
            "id": "chatcmpl-reasoning-length",
            "created": 1,
            "model": "opaque",
            "choices": [{
                "index": 0,
                "delta": {"reasoning_content": "finished plan"},
                "finish_reason": null
            }]
        }),
        json!({
            "id": "chatcmpl-reasoning-length",
            "created": 1,
            "model": "opaque",
            "choices": [{
                "index": 0,
                "delta": {"content": "partial answer"},
                "finish_reason": null
            }]
        }),
        json!({
            "id": "chatcmpl-reasoning-length",
            "created": 1,
            "model": "opaque",
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "length"
            }]
        }),
    ];

    let mut events = chunks
        .iter()
        .flat_map(|chunk| translator.translate_event(chunk).unwrap())
        .collect::<Vec<_>>();
    events.extend(translator.finish().unwrap());

    let incomplete = events
        .iter()
        .find(|event| event["type"] == "response.incomplete")
        .expect("length should emit response.incomplete at stream termination");
    assert_eq!(incomplete["response"]["output"][0]["type"], "reasoning");
    assert_eq!(incomplete["response"]["output"][0]["status"], "completed");
    assert_eq!(incomplete["response"]["output"][1]["type"], "message");
    assert_eq!(incomplete["response"]["output"][1]["status"], "incomplete");
}

#[test]
fn chat_stream_translator_assigns_unique_output_indexes_across_reasoning_text_and_tools() {
    let mut translator = StreamTranslator::new(
        UpstreamProtocol::ChatCompletions,
        UpstreamProtocol::Responses,
    )
    .expect("translator should exist");
    let chunks = [
        json!({
            "id": "chatcmpl-mixed",
            "created": 1,
            "model": "opaque",
            "choices": [{
                "index": 0,
                "delta": {"reasoning_content": "plan"},
                "finish_reason": null
            }]
        }),
        json!({
            "id": "chatcmpl-mixed",
            "created": 1,
            "model": "opaque",
            "choices": [{
                "index": 0,
                "delta": {"content": "answer"},
                "finish_reason": null
            }]
        }),
        json!({
            "id": "chatcmpl-mixed",
            "created": 1,
            "model": "opaque",
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "lookup",
                            "arguments": "{\"key\":\"value\"}"
                        }
                    }]
                },
                "finish_reason": null
            }]
        }),
        json!({
            "id": "chatcmpl-mixed",
            "created": 1,
            "model": "opaque",
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "tool_calls"
            }]
        }),
    ];

    let mut events = chunks
        .iter()
        .flat_map(|chunk| translator.translate_event(chunk).unwrap())
        .collect::<Vec<_>>();
    events.extend(translator.finish().unwrap());
    let added = events
        .iter()
        .filter(|event| event["type"] == "response.output_item.added")
        .map(|event| {
            (
                event["output_index"].as_u64().unwrap(),
                event["item"]["type"].as_str().unwrap().to_string(),
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(
        added,
        vec![
            (0, "reasoning".into()),
            (1, "message".into()),
            (2, "function_call".into()),
        ]
    );
    let completed = events
        .iter()
        .find(|event| event["type"] == "response.completed")
        .unwrap();
    assert_eq!(completed["response"]["output"][0]["type"], "reasoning");
    assert_eq!(completed["response"]["output"][1]["type"], "message");
    assert_eq!(completed["response"]["output"][2]["type"], "function_call");
}

#[test]
fn chat_response_length_becomes_incomplete_responses_payload() {
    let chat = json!({
        "id": "chatcmpl-length",
        "created": 1,
        "model": "opaque",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "partial"},
            "finish_reason": "length"
        }]
    });

    let response = chat_response_to_responses_payload(&chat).unwrap();

    assert_eq!(response["status"], "incomplete");
    assert_eq!(
        response["incomplete_details"]["reason"],
        "max_output_tokens"
    );
    assert_eq!(response["output"][0]["status"], "incomplete");
}

#[test]
fn chat_stream_length_emits_response_incomplete() {
    let mut translator = StreamTranslator::new(
        UpstreamProtocol::ChatCompletions,
        UpstreamProtocol::Responses,
    )
    .expect("translator should exist");
    let chunk = json!({
        "id": "chatcmpl-length",
        "created": 1,
        "model": "opaque",
        "choices": [{
            "index": 0,
            "delta": {"content": "partial"},
            "finish_reason": "length"
        }]
    });

    let finish_reason_events = translator.translate_event(&chunk).unwrap();

    assert!(!finish_reason_events.iter().any(|event| matches!(
        event["type"].as_str(),
        Some("response.completed" | "response.incomplete")
    )));
    let events = translator.finish().unwrap();
    assert!(!events
        .iter()
        .any(|event| event["type"] == "response.completed"));
    let incomplete = events
        .iter()
        .find(|event| event["type"] == "response.incomplete")
        .unwrap();
    assert_eq!(incomplete["response"]["status"], "incomplete");
    assert_eq!(
        incomplete["response"]["incomplete_details"]["reason"],
        "max_output_tokens"
    );
    assert_eq!(incomplete["response"]["output"][0]["status"], "incomplete");
    let output_done = events
        .iter()
        .find(|event| event["type"] == "response.output_item.done")
        .unwrap();
    assert_eq!(output_done["item"]["status"], "incomplete");
}

#[test]
fn chat_stream_translator_maps_completed_usage_to_responses_usage() {
    let mut translator = StreamTranslator::new(
        UpstreamProtocol::ChatCompletions,
        UpstreamProtocol::Responses,
    )
    .expect("translator should exist");

    let delta = json!({
        "id": "chatcmpl-stream",
        "created": 1,
        "model": "GLM-5.1",
        "choices": [{
            "index": 0,
            "delta": {
                "role": "assistant",
                "content": "OK"
            }
        }]
    });
    translator
        .translate_event(&delta)
        .expect("delta should translate");

    let final_chunk = json!({
        "id": "chatcmpl-stream",
        "created": 1,
        "model": "GLM-5.1",
        "choices": [{
            "index": 0,
            "delta": {},
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 2,
            "total_tokens": 12,
            "prompt_tokens_details": {
                "cached_tokens": 3
            },
            "completion_tokens_details": {
                "reasoning_tokens": 1
            }
        }
    });

    let finish_reason_events = translator
        .translate_event(&final_chunk)
        .expect("final chunk should translate");
    assert!(!finish_reason_events.iter().any(|event| matches!(
        event["type"].as_str(),
        Some("response.completed" | "response.incomplete")
    )));
    let events = translator.finish().expect("stream should finish");
    let completed = events
        .iter()
        .find(|event| event["type"] == "response.completed")
        .expect("expected response.completed event");

    assert_eq!(completed["response"]["usage"]["input_tokens"], 10);
    assert_eq!(completed["response"]["usage"]["output_tokens"], 2);
    assert_eq!(completed["response"]["usage"]["total_tokens"], 12);
    assert!(completed["response"]["usage"]
        .get("prompt_tokens")
        .is_none());
    assert!(completed["response"]["usage"]
        .get("completion_tokens")
        .is_none());
    assert_eq!(
        completed["response"]["usage"]["input_tokens_details"]["cached_tokens"],
        3
    );
    assert_eq!(
        completed["response"]["usage"]["output_tokens_details"]["reasoning_tokens"],
        1
    );
}

#[test]
fn chat_stream_defaults_required_responses_usage_details() {
    let mut translator = StreamTranslator::new(
        UpstreamProtocol::ChatCompletions,
        UpstreamProtocol::Responses,
    )
    .expect("translator should exist");

    translator
        .translate_event(&json!({
            "id": "chatcmpl-stream-defaults",
            "created": 1,
            "model": "opaque-model",
            "choices": [{
                "index": 0,
                "delta": {"content": "Complete"},
                "finish_reason": null
            }]
        }))
        .expect("delta should translate");
    translator
        .translate_event(&json!({
            "id": "chatcmpl-stream-defaults",
            "created": 1,
            "model": "opaque-model",
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 4,
                "completion_tokens": 2,
                "total_tokens": 6,
                "completion_tokens_details": {
                    "accepted_prediction_tokens": 1
                }
            }
        }))
        .expect("terminal chunk should translate");

    let events = translator.finish().expect("stream should finish");
    let completed = events
        .iter()
        .find(|event| event["type"] == "response.completed")
        .expect("expected response.completed event");
    let usage = &completed["response"]["usage"];
    assert_eq!(usage["input_tokens_details"]["cached_tokens"], 0);
    assert_eq!(usage["output_tokens_details"]["reasoning_tokens"], 0);
    assert_eq!(
        usage["output_tokens_details"]["accepted_prediction_tokens"],
        1
    );
}

#[test]
fn chat_stream_translator_rejects_multiple_choices() {
    let mut translator = StreamTranslator::new(
        UpstreamProtocol::ChatCompletions,
        UpstreamProtocol::Responses,
    )
    .expect("translator should exist");

    let chunk = json!({
        "id": "chatcmpl-stream",
        "object": "chat.completion.chunk",
        "choices": [
            {
                "index": 0,
                "delta": {
                    "role": "assistant",
                    "content": "Hi"
                },
                "finish_reason": null
            },
            {
                "index": 1,
                "delta": {
                    "role": "assistant",
                    "content": "Bye"
                },
                "finish_reason": null
            }
        ]
    });

    let error = translator
        .translate_event(&chunk)
        .expect_err("translation should fail");
    assert!(error
        .to_string()
        .contains("multiple chat completion choices are not supported"));
}

#[test]
fn responses_response_rejects_unknown_output_items() {
    let responses = json!({
        "id": "resp-1",
        "object": "response",
        "created": 1,
        "model": "gpt-4.1-mini",
        "output": [
            {
                "id": "unsupported_1",
                "type": "unsupported_output"
            }
        ]
    });

    let error = responses_response_to_chat_payload(&responses).expect_err("conversion should fail");
    assert!(error
        .to_string()
        .contains("unsupported responses output item type"));
}

#[test]
fn responses_response_rejects_non_assistant_output_roles() {
    let responses = json!({
        "id": "resp-1",
        "object": "response",
        "created": 1,
        "model": "gpt-4.1-mini",
        "output": [
            {
                "id": "msg_1",
                "type": "message",
                "role": "user",
                "content": [
                    {
                        "type": "output_text",
                        "text": "Hi",
                        "annotations": []
                    }
                ]
            }
        ]
    });

    let error = responses_response_to_chat_payload(&responses).expect_err("conversion should fail");
    assert!(error
        .to_string()
        .contains("unsupported responses output role"));
}

#[test]
fn responses_stream_translator_rejects_multiple_assistant_messages() {
    let mut translator = StreamTranslator::new(
        UpstreamProtocol::Responses,
        UpstreamProtocol::ChatCompletions,
    )
    .expect("translator should exist");

    let first_message_added = json!({
        "type": "response.output_item.added",
        "response_id": "resp-1",
        "output_index": 0,
        "item": {
            "id": "msg-1",
            "type": "message",
            "status": "in_progress",
            "role": "assistant",
            "content": []
        }
    });
    translator
        .translate_event(&first_message_added)
        .expect("first message should translate");

    let second_message_added = json!({
        "type": "response.output_item.added",
        "response_id": "resp-1",
        "output_index": 1,
        "item": {
            "id": "msg-2",
            "type": "message",
            "status": "in_progress",
            "role": "assistant",
            "content": []
        }
    });

    let error = translator
        .translate_event(&second_message_added)
        .expect_err("translation should fail");
    assert!(error
        .to_string()
        .contains("multiple assistant messages are not supported"));
}

#[test]
fn chat_request_to_responses_forwards_reasoning_effort() {
    let input = json!({
        "model": "gpt-4",
        "messages": [{"role": "user", "content": "hello"}],
        "reasoning_effort": "high"
    });
    let result = chat_request_to_responses_payload(&input).unwrap();
    let reasoning = result.get("reasoning").and_then(|r| r.as_object()).unwrap();
    assert_eq!(
        reasoning.get("effort").and_then(|v| v.as_str()),
        Some("high")
    );
}

#[test]
fn responses_request_to_chat_forwards_reasoning_effort() {
    let input = json!({
        "model": "gpt-4",
        "input": "hello",
        "reasoning": {"effort": "medium"}
    });
    let result = responses_request_to_chat_payload(&input).unwrap();
    assert_eq!(
        result.get("reasoning_effort").and_then(|v| v.as_str()),
        Some("medium")
    );
}

#[test]
fn chat_request_without_reasoning_effort_is_unchanged() {
    let input = json!({
        "model": "gpt-4",
        "messages": [{"role": "user", "content": "hello"}]
    });
    let result = chat_request_to_responses_payload(&input).unwrap();
    assert!(result.get("reasoning").is_none());
}

#[test]
fn responses_request_without_reasoning_effort_is_unchanged() {
    let input = json!({
        "model": "gpt-4",
        "input": "hello"
    });
    let result = responses_request_to_chat_payload(&input).unwrap();
    assert!(result.get("reasoning_effort").is_none());
}

// --- P1: Field forwarding tests (RED — these fields are currently dropped) ---

#[test]
fn chat_request_forwards_client_metadata_to_responses() {
    let input = json!({
        "model": "gpt-4",
        "messages": [{"role": "user", "content": "hello"}],
        "client_metadata": {
            "x-codex-turn-metadata": "{\"session_id\":\"abc\",\"turn_id\":\"def\"}"
        }
    });
    let result = chat_request_to_responses_payload(&input).unwrap();
    assert!(
        result.get("client_metadata").is_some(),
        "client_metadata should be forwarded in chat→responses conversion"
    );
}

#[test]
fn responses_request_forwards_client_metadata_to_chat() {
    let input = json!({
        "model": "gpt-4",
        "input": "hello",
        "client_metadata": {
            "x-codex-turn-metadata": "{\"session_id\":\"abc\",\"turn_id\":\"def\"}"
        }
    });
    let result = responses_request_to_chat_payload(&input).unwrap();
    // ChatCompletions API doesn't have client_metadata, so it should be dropped
    // gracefully (not crash). The test just verifies no error.
    assert_eq!(result["model"], "gpt-4");
}

#[test]
fn chat_request_forwards_stop_to_responses() {
    let input = json!({
        "model": "gpt-4",
        "messages": [{"role": "user", "content": "hello"}],
        "stop": ["\n"]
    });
    let result = chat_request_to_responses_payload(&input).unwrap();
    // stop is already copy_field'd, verify it's present
    assert!(
        result.get("stop").is_some(),
        "stop should be forwarded in chat→responses conversion"
    );
}

#[test]
fn responses_request_forwards_parallel_tool_calls_to_chat() {
    let input = json!({
        "model": "gpt-4",
        "input": "hello",
        "parallel_tool_calls": true
    });
    let result = responses_request_to_chat_payload(&input).unwrap();
    assert!(
        result.get("parallel_tool_calls").is_some(),
        "parallel_tool_calls should be forwarded in responses→chat conversion"
    );
}

#[test]
fn chat_request_forwards_parallel_tool_calls_to_responses() {
    let input = json!({
        "model": "gpt-4",
        "messages": [{"role": "user", "content": "hello"}],
        "parallel_tool_calls": true
    });
    let result = chat_request_to_responses_payload(&input).unwrap();
    assert!(
        result.get("parallel_tool_calls").is_some(),
        "parallel_tool_calls should be forwarded in chat→responses conversion"
    );
}

mod stream_aggregate_tests {
    use super::*;
    use chat_responses_codex::protocol::stream_aggregate::{
        MAX_STREAM_AGGREGATE_FRAME_BYTES, MAX_STREAM_AGGREGATE_TOTAL_BYTES,
    };

    fn expect_complete(result: StreamAggregateResult) -> serde_json::Value {
        match result {
            StreamAggregateResult::Complete(value) => value,
            StreamAggregateResult::Pending => panic!("expected a complete aggregate"),
        }
    }

    #[test]
    fn stream_aggregate_chat_handles_sse_framing_utf8_and_usage_tail() {
        let mut aggregator = StreamResponseAggregator::new(UpstreamProtocol::ChatCompletions);
        let metadata_frame = concat!(
            ": keepalive\r\n\r\n",
            "data:{\"id\":\"chatcmpl-aggregate\",\"object\":\"chat.completion.chunk\",\r\n",
            "data: \"created\":17,\"model\":\"opaque/runtime\",\"service_tier\":\"priority\",",
            "\"system_fingerprint\":\"fp_opaque\",\"choices\":[{\"index\":0,",
            "\"delta\":{\"role\":\"assistant\"},\"finish_reason\":null}]}\r\n\r\n"
        );
        let text_frame = format!(
            "data: {}\r\n\r\n",
            json!({
                "id": "chatcmpl-aggregate",
                "object": "chat.completion.chunk",
                "created": 17,
                "model": "opaque/runtime",
                "choices": [{
                    "index": 0,
                    "delta": {"content": "\u{4f60}\u{597d}"},
                    "finish_reason": null
                }]
            })
        );
        let finish_frame = format!(
            "data: {}\n\n",
            json!({
                "id": "chatcmpl-aggregate",
                "choices": [{
                    "index": 0,
                    "delta": {},
                    "finish_reason": "stop",
                    "logprobs": {"content": [{"token": "done"}]}
                }]
            })
        );
        let usage_frame = format!(
            "data:{}\n\n",
            json!({
                "id": "chatcmpl-aggregate",
                "choices": [],
                "usage": {
                    "prompt_tokens": 3,
                    "completion_tokens": 2,
                    "total_tokens": 5
                }
            })
        );
        let before_done = format!("{metadata_frame}{text_frame}{finish_frame}{usage_frame}");

        for byte in before_done.as_bytes().chunks(1) {
            assert_eq!(
                aggregator.push(byte).expect("fragment should parse"),
                StreamAggregateResult::Pending,
                "finish_reason must not complete before a possible usage tail"
            );
        }
        let value = expect_complete(
            aggregator
                .push(b"data:[DONE]\r\n\r\n")
                .expect("done should complete"),
        );

        assert_eq!(value["id"], "chatcmpl-aggregate");
        assert_eq!(value["object"], "chat.completion");
        assert_eq!(value["created"], 17);
        assert_eq!(value["model"], "opaque/runtime");
        assert_eq!(value["service_tier"], "priority");
        assert_eq!(value["system_fingerprint"], "fp_opaque");
        assert_eq!(value["choices"][0]["index"], 0);
        assert_eq!(value["choices"][0]["message"]["role"], "assistant");
        assert_eq!(
            value["choices"][0]["message"]["content"],
            "\u{4f60}\u{597d}"
        );
        assert_eq!(value["choices"][0]["finish_reason"], "stop");
        assert_eq!(
            value["choices"][0]["logprobs"]["content"][0]["token"],
            "done"
        );
        assert_eq!(value["usage"]["completion_tokens"], 2);
    }

    #[test]
    fn stream_aggregate_chat_orders_sparse_choices_and_parallel_tools() {
        let mut aggregator = StreamResponseAggregator::new(UpstreamProtocol::ChatCompletions);
        let events = [
            json!({
                "id": "chatcmpl-tools",
                "created": 21,
                "model": "opaque",
                "choices": [
                    {"index": 2, "delta": {
                        "role": "assistant",
                        "reasoning_content": "plan ",
                        "content": "answer "
                    }, "finish_reason": null},
                    {"index": 0, "delta": {"refusal": "not "}, "finish_reason": null}
                ]
            }),
            json!({
                "id": "chatcmpl-tools",
                "choices": [{"index": 2, "delta": {"tool_calls": [
                    {"index": 5, "id": "call_", "type": "function", "function": {
                        "name": "look", "arguments": "{\"q\":"
                    }},
                    {"index": 1, "id": "call_b", "type": "function", "function": {
                        "name": "sum", "arguments": "{\"x\":"
                    }}
                ]}, "finish_reason": null}]
            }),
            json!({
                "id": "chatcmpl-tools",
                "choices": [
                    {"index": 2, "delta": {
                        "reasoning_content": "then act",
                        "content": "done",
                        "tool_calls": [
                            {"index": 5, "id": "five", "function": {
                                "name": "up", "arguments": "\"v\"}"
                            }},
                            {"index": 1, "id": "_1", "function": {
                                "name": "_all", "arguments": "1}"
                            }}
                        ]
                    }, "finish_reason": "tool_calls"},
                    {"index": 0, "delta": {"refusal": "allowed"}, "finish_reason": "stop"}
                ]
            }),
        ];
        for event in events {
            let frame = format!("data: {event}\n\n");
            assert_eq!(
                aggregator.push(frame.as_bytes()).unwrap(),
                StreamAggregateResult::Pending
            );
        }
        let value = expect_complete(aggregator.push(b"data: [DONE]\n\n").unwrap());

        assert_eq!(value["choices"][0]["index"], 0);
        assert_eq!(value["choices"][0]["message"]["refusal"], "not allowed");
        assert_eq!(value["choices"][1]["index"], 2);
        assert_eq!(
            value["choices"][1]["message"]["reasoning_content"],
            "plan then act"
        );
        assert_eq!(value["choices"][1]["message"]["content"], "answer done");
        let tools = value["choices"][1]["message"]["tool_calls"]
            .as_array()
            .unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["index"], 1);
        assert_eq!(tools[0]["id"], "call_b_1");
        assert_eq!(tools[0]["function"]["name"], "sum_all");
        assert_eq!(tools[0]["function"]["arguments"], "{\"x\":1}");
        assert_eq!(tools[1]["index"], 5);
        assert_eq!(tools[1]["id"], "call_five");
        assert_eq!(tools[1]["function"]["name"], "lookup");
        assert_eq!(tools[1]["function"]["arguments"], "{\"q\":\"v\"}");
    }

    #[test]
    fn stream_aggregate_chat_keeps_stable_tool_identity_repeats_idempotent() {
        let mut aggregator = StreamResponseAggregator::new(UpstreamProtocol::ChatCompletions);
        for event in [
            json!({
                "id": "chatcmpl-repeat",
                "created": 21,
                "model": "opaque",
                "choices": [{"index": 0, "delta": {"tool_calls": [{
                    "index": 0,
                    "id": "call_stable",
                    "type": "function",
                    "function": {"name": "get_weather", "arguments": "a"}
                }]}, "finish_reason": null}]
            }),
            json!({
                "id": "chatcmpl-repeat",
                "choices": [{"index": 0, "delta": {"tool_calls": [{
                    "index": 0,
                    "id": "call_stable",
                    "function": {"name": "get_weather", "arguments": "a"}
                }]}, "finish_reason": "tool_calls"}]
            }),
        ] {
            let frame = format!("data: {event}\n\n");
            assert_eq!(
                aggregator.push(frame.as_bytes()).unwrap(),
                StreamAggregateResult::Pending
            );
        }
        let value = expect_complete(aggregator.push(b"data: [DONE]\n\n").unwrap());
        let tool = &value["choices"][0]["message"]["tool_calls"][0];

        assert_eq!(tool["id"], "call_stable");
        assert_eq!(tool["function"]["name"], "get_weather");
        assert_eq!(tool["function"]["arguments"], "aa");
    }

    #[test]
    fn stream_aggregate_chat_accepts_cumulative_tool_identity_snapshots() {
        let mut aggregator = StreamResponseAggregator::new(UpstreamProtocol::ChatCompletions);
        for event in [
            json!({
                "id": "chatcmpl-cumulative",
                "choices": [{"index": 0, "delta": {"tool_calls": [{
                    "index": 0,
                    "id": "call_",
                    "type": "function",
                    "function": {"name": "look", "arguments": "a"}
                }]}, "finish_reason": null}]
            }),
            json!({
                "id": "chatcmpl-cumulative",
                "choices": [{"index": 0, "delta": {"tool_calls": [{
                    "index": 0,
                    "id": "call_cumulative",
                    "function": {"name": "lookup", "arguments": "a"}
                }]}, "finish_reason": null}]
            }),
            json!({
                "id": "chatcmpl-cumulative",
                "choices": [{"index": 0, "delta": {"tool_calls": [{
                    "index": 0,
                    "id": "call_",
                    "function": {"name": "look", "arguments": "a"}
                }]}, "finish_reason": "tool_calls"}]
            }),
        ] {
            let frame = format!("data: {event}\n\n");
            assert_eq!(
                aggregator.push(frame.as_bytes()).unwrap(),
                StreamAggregateResult::Pending
            );
        }
        let value = expect_complete(aggregator.push(b"data: [DONE]\n\n").unwrap());
        let tool = &value["choices"][0]["message"]["tool_calls"][0];

        assert_eq!(tool["id"], "call_cumulative");
        assert_eq!(tool["function"]["name"], "lookup");
        assert_eq!(tool["function"]["arguments"], "aaa");
    }

    #[test]
    fn stream_aggregate_chat_appends_distinct_tool_identity_fragments() {
        let mut aggregator = StreamResponseAggregator::new(UpstreamProtocol::ChatCompletions);
        for (name, finish_reason) in [("get_", json!(null)), ("geo", json!("tool_calls"))] {
            let event = json!({
                "id": "chatcmpl-fragments",
                "choices": [{"index": 0, "delta": {"tool_calls": [{
                    "index": 0,
                    "id": "call_fragments",
                    "type": "function",
                    "function": {"name": name, "arguments": ""}
                }]}, "finish_reason": finish_reason}]
            });
            let frame = format!("data: {event}\n\n");
            assert_eq!(
                aggregator.push(frame.as_bytes()).unwrap(),
                StreamAggregateResult::Pending
            );
        }
        let value = expect_complete(aggregator.push(b"data: [DONE]\n\n").unwrap());
        let tool = &value["choices"][0]["message"]["tool_calls"][0];

        assert_eq!(tool["id"], "call_fragments");
        assert_eq!(tool["function"]["name"], "get_geo");
    }

    #[test]
    fn stream_aggregate_chat_legacy_function_call_completes_on_semantic_eof() {
        let mut aggregator = StreamResponseAggregator::new(UpstreamProtocol::ChatCompletions);
        let first = json!({
            "id": "chatcmpl-legacy",
            "created": 22,
            "model": "opaque",
            "choices": [{"index": 0, "delta": {"function_call": {
                "name": "leg", "arguments": "{\"value\":"
            }}, "finish_reason": null}]
        });
        assert_eq!(
            aggregator
                .push(format!("data: {first}\n\n").as_bytes())
                .unwrap(),
            StreamAggregateResult::Pending
        );
        let final_event = json!({
            "id": "chatcmpl-legacy",
            "choices": [{"index": 0, "delta": {"function_call": {
                "name": "acy", "arguments": "1}"
            }}, "finish_reason": "function_call"}]
        });
        assert_eq!(
            aggregator
                .push(format!("data: {final_event}").as_bytes())
                .unwrap(),
            StreamAggregateResult::Pending
        );

        let value = aggregator
            .finish()
            .expect("semantic finish makes a clean EOF complete");
        assert_eq!(
            value["choices"][0]["message"]["function_call"]["name"],
            "legacy"
        );
        assert_eq!(
            value["choices"][0]["message"]["function_call"]["arguments"],
            "{\"value\":1}"
        );
        assert_eq!(value["choices"][0]["finish_reason"], "function_call");
    }

    #[test]
    fn stream_aggregate_chat_rejects_truncated_eof() {
        let mut aggregator = StreamResponseAggregator::new(UpstreamProtocol::ChatCompletions);
        let frame = format!(
            "data: {}\n\n",
            json!({
                "id": "chatcmpl-truncated",
                "choices": [{"index": 0, "delta": {"content": "partial"}, "finish_reason": null}]
            })
        );
        assert_eq!(
            aggregator.push(frame.as_bytes()).unwrap(),
            StreamAggregateResult::Pending
        );
        assert!(aggregator.finish().is_err());

        let mut partial = StreamResponseAggregator::new(UpstreamProtocol::ChatCompletions);
        assert_eq!(
            partial.push(b"data: {\"choices\":[{\"index\":0").unwrap(),
            StreamAggregateResult::Pending
        );
        assert!(partial.finish().is_err());
    }

    #[test]
    fn stream_aggregate_completion_is_emitted_once_and_rejects_post_complete_calls() {
        let mut aggregator = StreamResponseAggregator::new(UpstreamProtocol::ChatCompletions);
        let terminal = format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({
                "id": "chatcmpl-move-only",
                "choices": [{
                    "index": 0,
                    "delta": {"content": "done"},
                    "finish_reason": "stop"
                }]
            })
        );

        let value = expect_complete(aggregator.push(terminal.as_bytes()).unwrap());
        assert_eq!(value["choices"][0]["message"]["content"], "done");

        let push_error = aggregator
            .push(b": post-complete\n\n")
            .expect_err("completion must not be cloned for later pushes");
        assert!(matches!(
            push_error,
            chat_responses_codex::protocol::ProtocolError::InvalidUpstreamStream {
                kind: chat_responses_codex::protocol::UpstreamStreamErrorKind::Decode,
                ..
            }
        ));
        let finish_error = aggregator
            .finish()
            .expect_err("completion must not be rebuilt by finish");
        assert!(matches!(
            finish_error,
            chat_responses_codex::protocol::ProtocolError::InvalidUpstreamStream {
                kind: chat_responses_codex::protocol::UpstreamStreamErrorKind::Decode,
                ..
            }
        ));
    }

    #[test]
    fn stream_aggregate_responses_returns_authoritative_completed_snapshot() {
        let mut aggregator = StreamResponseAggregator::new(UpstreamProtocol::Responses);
        for event in [
            json!({"type": "response.created", "response": {
                "id": "resp-aggregate", "status": "in_progress", "output": []
            }}),
            json!({"type": "response.output_text.delta", "output_index": 1,
                "content_index": 0, "delta": "ignored partial"}),
            json!({"type": "response.function_call_arguments.delta", "output_index": 3,
                "item_id": "fc-2", "delta": "{\"partial\":"}),
        ] {
            let frame = format!("data: {event}\n\n");
            assert_eq!(
                aggregator.push(frame.as_bytes()).unwrap(),
                StreamAggregateResult::Pending
            );
        }
        let expected = json!({
            "id": "resp-aggregate",
            "object": "response",
            "created_at": 31,
            "status": "completed",
            "model": "opaque/responses",
            "output": [
                {"id": "rs-1", "type": "reasoning", "status": "completed",
                    "content": [{"type": "reasoning_text", "text": "plan"}], "summary": []},
                {"id": "msg-1", "type": "message", "status": "completed", "role": "assistant",
                    "content": [{"type": "output_text", "text": "answer", "annotations": []}]},
                {"id": "fc-1", "type": "function_call", "status": "completed",
                    "call_id": "call-1", "name": "first", "arguments": "{\"x\":1}"},
                {"id": "fc-2", "type": "function_call", "status": "completed",
                    "call_id": "call-2", "name": "second", "arguments": "{\"y\":2}"}
            ],
            "usage": {"input_tokens": 4, "output_tokens": 6, "total_tokens": 10}
        });
        let terminal = format!(
            "event: response.completed\r\ndata: {}\r\n\r\n",
            json!({"type": "response.completed", "response": expected.clone()})
        );
        let value = expect_complete(aggregator.push(terminal.as_bytes()).unwrap());
        assert_eq!(value, expected);
    }

    #[test]
    fn stream_aggregate_responses_accepts_incomplete_terminal_snapshot() {
        let mut aggregator = StreamResponseAggregator::new(UpstreamProtocol::Responses);
        let expected = json!({
            "id": "resp-incomplete",
            "object": "response",
            "status": "incomplete",
            "model": "opaque",
            "output": [{"id": "msg-1", "type": "message", "status": "incomplete",
                "role": "assistant", "content": [{"type": "output_text", "text": "partial"}]}],
            "incomplete_details": {"reason": "max_output_tokens"},
            "usage": {"input_tokens": 3, "output_tokens": 2, "total_tokens": 5}
        });
        let frame = format!(
            "data: {}",
            json!({"type": "response.incomplete", "response": expected.clone()})
        );

        assert_eq!(
            aggregator.push(frame.as_bytes()).unwrap(),
            StreamAggregateResult::Pending
        );
        let value = aggregator.finish().unwrap();
        assert_eq!(value, expected);
    }

    #[test]
    fn stream_aggregate_responses_rejects_error_events_and_envelopes() {
        let frames = [
            format!(
                "data: {}\n\n",
                json!({"type": "response.failed", "response": {
                    "id": "resp-failed", "status": "failed", "error": {"message": "failed"}
                }})
            ),
            format!(
                "event: error\ndata: {}\n\n",
                json!({"type": "error", "message": "failed"})
            ),
            format!("data: {}\n\n", json!({"error": {"message": "failed"}})),
            "data: {not-json}\n\n".to_string(),
        ];

        for frame in frames {
            let mut aggregator = StreamResponseAggregator::new(UpstreamProtocol::Responses);
            assert!(
                aggregator.push(frame.as_bytes()).is_err(),
                "frame must fail: {frame}"
            );
        }
    }

    #[test]
    fn stream_aggregate_responses_rejects_error_event_after_terminal_in_same_push() {
        let mut aggregator = StreamResponseAggregator::new(UpstreamProtocol::Responses);
        let terminal = json!({
            "type": "response.completed",
            "response": {
                "id": "resp-terminal-error",
                "object": "response",
                "status": "completed",
                "output": []
            }
        });
        let stream = format!("event: response.completed\ndata: {terminal}\n\nevent: error\n\n");

        let error = aggregator
            .push(stream.as_bytes())
            .expect_err("post-terminal error event must not be ignored");

        assert!(matches!(
            error,
            chat_responses_codex::protocol::ProtocolError::InvalidUpstreamStream {
                kind: chat_responses_codex::protocol::UpstreamStreamErrorKind::UpstreamEvent,
                ..
            }
        ));
    }

    #[test]
    fn stream_aggregate_responses_consumes_benign_same_push_terminal_tail() {
        let mut aggregator = StreamResponseAggregator::new(UpstreamProtocol::Responses);
        let expected = json!({
            "id": "resp-terminal-tail",
            "object": "response",
            "status": "completed",
            "output": []
        });
        let terminal = json!({
            "type": "response.completed",
            "response": expected.clone()
        });
        let stream = format!(
            "event: response.completed\ndata: {terminal}\n\n: keepalive\n\ndata: [DONE]\n\n"
        );
        let mut observed_payloads = Vec::new();

        let value = expect_complete(
            aggregator
                .push_observing(stream.as_bytes(), |event| {
                    observed_payloads.push(event.data().to_owned());
                })
                .unwrap(),
        );

        assert_eq!(value, expected);
        assert_eq!(observed_payloads.len(), 2);
        assert_eq!(observed_payloads[1], "[DONE]");
        assert!(aggregator.push(b": after-complete\n\n").is_err());
    }

    #[test]
    fn stream_aggregate_rejects_header_only_error_event() {
        let mut aggregator = StreamResponseAggregator::new(UpstreamProtocol::Responses);
        assert!(aggregator.push(b"event: error\n\n").is_err());
    }

    #[test]
    fn stream_aggregate_rejects_header_only_response_failed_event() {
        let mut aggregator = StreamResponseAggregator::new(UpstreamProtocol::Responses);
        assert!(aggregator
            .push(b"event: response.failed\ndata: {}\n\n")
            .is_err());
    }

    #[test]
    fn stream_aggregate_accepts_terminal_type_from_sse_event_header() {
        let mut aggregator = StreamResponseAggregator::new(UpstreamProtocol::Responses);
        let expected = json!({
            "id": "resp-header-terminal",
            "object": "response",
            "status": "completed",
            "model": "opaque",
            "output": []
        });
        let frame = format!(
            "event: response.completed\ndata: {}\n\n",
            json!({"response": expected.clone()})
        );

        let value = expect_complete(aggregator.push(frame.as_bytes()).unwrap());
        assert_eq!(value, expected);
    }

    #[test]
    fn stream_aggregate_responses_rejects_eof_without_terminal_snapshot() {
        let mut aggregator = StreamResponseAggregator::new(UpstreamProtocol::Responses);
        let frame = format!(
            "data: {}\n\n",
            json!({"type": "response.output_text.delta", "output_index": 0, "delta": "partial"})
        );
        assert_eq!(
            aggregator.push(frame.as_bytes()).unwrap(),
            StreamAggregateResult::Pending
        );
        assert!(aggregator.finish().is_err());
    }

    fn comment_frame(total_bytes: usize) -> Vec<u8> {
        assert!(total_bytes >= 3);
        let mut frame = Vec::with_capacity(total_bytes);
        frame.push(b':');
        frame.resize(total_bytes - 2, b'x');
        frame.extend_from_slice(b"\n\n");
        frame
    }

    #[test]
    fn stream_aggregate_enforces_exact_frame_and_total_byte_bounds() {
        let exact_frame = comment_frame(MAX_STREAM_AGGREGATE_FRAME_BYTES);
        let mut frame_ok = StreamResponseAggregator::new(UpstreamProtocol::ChatCompletions);
        assert_eq!(
            frame_ok.push(&exact_frame).unwrap(),
            StreamAggregateResult::Pending
        );

        let oversized_frame = comment_frame(MAX_STREAM_AGGREGATE_FRAME_BYTES + 1);
        let mut frame_too_large = StreamResponseAggregator::new(UpstreamProtocol::ChatCompletions);
        assert!(frame_too_large.push(&oversized_frame).is_err());

        assert_eq!(
            MAX_STREAM_AGGREGATE_TOTAL_BYTES % MAX_STREAM_AGGREGATE_FRAME_BYTES,
            0
        );
        let mut total = StreamResponseAggregator::new(UpstreamProtocol::ChatCompletions);
        for _ in 0..(MAX_STREAM_AGGREGATE_TOTAL_BYTES / MAX_STREAM_AGGREGATE_FRAME_BYTES) {
            assert_eq!(
                total.push(&exact_frame).unwrap(),
                StreamAggregateResult::Pending
            );
        }
        assert!(total.push(b":").is_err());
    }
}
