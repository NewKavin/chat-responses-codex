use chat2responses_gateway::protocol::{
    chat_request_to_responses_payload, chat_response_to_responses_payload,
    responses_request_to_chat_payload, responses_response_to_chat_payload,
};
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
    assert_eq!(converted["tools"][0]["function"]["parameters"]["type"], "object");
}

#[test]
fn responses_request_ignores_unsupported_tools_for_chat_payload() {
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

    let converted = responses_request_to_chat_payload(&responses).expect("conversion should work");

    assert_eq!(converted["tools"][0]["type"], "function");
    assert_eq!(converted["tools"][0]["function"]["name"], "get_weather");
    assert!(converted.get("tool_choice").is_none());
}

#[test]
fn responses_request_drops_tool_choice_when_no_supported_tools_remain() {
    let responses = json!({
        "model": "gpt-4.1-mini",
        "input": "Hello",
        "tools": [
            {
                "type": "web_search"
            }
        ],
        "tool_choice": "required"
    });

    let converted = responses_request_to_chat_payload(&responses).expect("conversion should work");

    assert!(converted.get("tools").is_none());
    assert!(converted.get("tool_choice").is_none());
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
    assert_eq!(converted["messages"][2]["tool_calls"][0]["function"]["name"], "get_weather");
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

    let converted = chat_response_to_responses_payload(&chat_response).expect("conversion should work");

    assert_eq!(converted["id"], "chatcmpl-1");
    assert_eq!(converted["object"], "response");
    assert_eq!(converted["output"][0]["type"], "function_call");
    assert_eq!(converted["output"][0]["call_id"], "call_1");
    assert_eq!(converted["output"][0]["name"], "get_weather");
    assert_eq!(
        converted["output"][0]["arguments"],
        "{\"city\":\"Paris\"}"
    );
    assert_eq!(converted["usage"]["prompt_tokens"], 1);
    assert_eq!(converted["usage"]["completion_tokens"], 0);
    assert_eq!(converted["usage"]["total_tokens"], 1);
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
    assert_eq!(converted["choices"][0]["message"]["tool_calls"][0]["id"], "call_1");
    assert_eq!(
        converted["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
        "get_weather"
    );
    assert_eq!(converted["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(converted["usage"]["prompt_tokens"], 1);
    assert_eq!(converted["usage"]["completion_tokens"], 0);
    assert_eq!(converted["usage"]["total_tokens"], 1);
}
