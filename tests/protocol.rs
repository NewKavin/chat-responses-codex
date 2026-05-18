use chat2responses_gateway::protocol::{
    chat_request_to_responses_payload, responses_request_to_chat_payload,
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
