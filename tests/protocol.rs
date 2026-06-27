use chat_responses_codex::protocol::{
    chat_request_to_responses_payload, chat_response_to_responses_payload,
    responses_request_to_chat_payload, responses_response_to_chat_payload, StreamTranslator,
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
fn responses_request_rejects_unknown_input_items_for_chat_payload() {
    let responses = json!({
        "model": "gpt-4.1-mini",
        "input": [
            {
                "type": "reasoning"
            }
        ]
    });

    let error = responses_request_to_chat_payload(&responses).expect_err("conversion should fail");
    assert!(error
        .to_string()
        .contains("unsupported responses input item"));
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
            "total_tokens": 1
        }
    });

    let converted =
        chat_response_to_responses_payload(&chat_response).expect("conversion should work");

    assert_eq!(converted["id"], "chatcmpl-1");
    assert_eq!(converted["object"], "response");
    assert_eq!(converted["output"][0]["type"], "function_call");
    assert_eq!(converted["output"][0]["call_id"], "call_1");
    assert_eq!(converted["output"][0]["name"], "get_weather");
    assert_eq!(converted["output"][0]["arguments"], "{\"city\":\"Paris\"}");
    assert_eq!(converted["usage"]["prompt_tokens"], 1);
    assert_eq!(converted["usage"]["completion_tokens"], 0);
    assert_eq!(converted["usage"]["total_tokens"], 1);
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
            "total_tokens": 1
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

    let error =
        responses_response_to_chat_payload(&responses).expect_err("conversion should fail");
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
    assert_eq!(reasoning.get("effort").and_then(|v| v.as_str()), Some("high"));
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
