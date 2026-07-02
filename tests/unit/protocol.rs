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
