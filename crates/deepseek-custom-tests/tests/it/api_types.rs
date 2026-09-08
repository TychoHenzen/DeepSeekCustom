//! Unit tests for `deepseek_custom::api::types`, moved out of the
//! production module as part of the two-crate workspace split.

use deepseek_custom::api::types::{
    ChatRequest, ChatResponse, Content, ContentPart, Message, Role, StreamChunk, Usage,
};
use deepseek_custom::effort::Effort;

#[test]
fn chat_request_serializes_correctly() {
    let req = ChatRequest {
        model: "deepseek-v4-flash".into(),
        messages: vec![Message {
            role: Role::User,
            content: Some(Content::text("hello")),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }],
        tools: None,
        tool_choice: None,
        stream: false,
        temperature: Some(0.7),
        max_tokens: Some(1024),
        thinking: None,
        thinking_mode: None,
        reasoning_effort: None,
        response_format: None,
        effort: None,
    };

    let json = serde_json::to_string(&req).expect("serialize");
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");

    assert_eq!(parsed["model"], "deepseek-v4-flash");
    assert_eq!(parsed["messages"][0]["role"], "user");
    assert_eq!(parsed["messages"][0]["content"], "hello");
    assert_eq!(parsed["stream"], false);
    assert_eq!(parsed["temperature"], 0.7);
    assert_eq!(parsed["max_tokens"], 1024);
    assert!(parsed.get("thinking").is_none());
    assert!(parsed.get("thinking_mode").is_none());
    assert!(parsed.get("response_format").is_none());
    assert_eq!(
        json,
        r#"{"model":"deepseek-v4-flash","messages":[{"role":"user","content":"hello"}],"stream":false,"temperature":0.7,"max_tokens":1024}"#
    );
}

#[test]
fn thinking_enabled_serializes_correctly() {
    let req = ChatRequest {
        model: "deepseek-v4-flash".into(),
        messages: vec![Message {
            role: Role::User,
            content: Some(Content::text("hello")),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }],
        tools: None,
        tool_choice: None,
        stream: true,
        temperature: Some(0.7),
        max_tokens: Some(4096),
        thinking: None,
        thinking_mode: Some("thinking".into()),
        reasoning_effort: None,
        response_format: None,
        effort: None,
    };

    let json = serde_json::to_string(&req).expect("serialize");
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");

    assert_eq!(parsed["thinking_mode"], "thinking");
    assert!(parsed.get("thinking").is_none());
}

#[test]
fn thinking_disabled_serializes_correctly() {
    let req = ChatRequest {
        model: "deepseek-v4-flash".into(),
        messages: vec![Message {
            role: Role::User,
            content: Some(Content::text("hello")),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }],
        tools: None,
        tool_choice: None,
        stream: true,
        temperature: Some(0.7),
        max_tokens: Some(4096),
        thinking: None,
        thinking_mode: Some("non-thinking".into()),
        reasoning_effort: None,
        response_format: None,
        effort: None,
    };

    let json = serde_json::to_string(&req).expect("serialize");
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");

    assert_eq!(parsed["thinking_mode"], "non-thinking");
    assert!(parsed.get("thinking").is_none());
}

#[test]
fn reasoning_effort_is_absent_when_none() {
    let req = ChatRequest {
        model: "deepseek-v4-flash".into(),
        messages: vec![Message {
            role: Role::User,
            content: Some(Content::text("hello")),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }],
        tools: None,
        tool_choice: None,
        stream: false,
        temperature: Some(0.7),
        max_tokens: Some(1024),
        thinking: None,
        thinking_mode: None,
        reasoning_effort: None,
        response_format: None,
        effort: None,
    };

    let json = serde_json::to_string(&req).expect("serialize");
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");

    assert!(parsed.get("reasoning_effort").is_none());
}

#[test]
fn effort_field_never_appears_on_the_wire_even_when_set() {
    let req = ChatRequest {
        model: "deepseek-v4-flash".into(),
        messages: vec![Message {
            role: Role::User,
            content: Some(Content::text("hello")),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }],
        tools: None,
        tool_choice: None,
        stream: false,
        temperature: Some(0.7),
        max_tokens: Some(1024),
        thinking: None,
        thinking_mode: None,
        reasoning_effort: None,
        response_format: None,
        effort: Some(Effort::Max),
    };

    let json = serde_json::to_string(&req).expect("serialize");
    assert!(!json.contains("effort"));
}

#[test]
fn stream_chunk_parses_reasoning_content() {
    let data = r#"{"id":"chatcmpl-xyz","object":"chat.completion.chunk","created":1716902400,"model":"deepseek-v4-flash","choices":[{"index":0,"delta":{"content":null,"reasoning_content":"First I will think about this problem carefully"},"finish_reason":null}]}"#;
    let chunk: StreamChunk = serde_json::from_str(data).expect("parse");

    let choices = chunk.choices.as_ref().unwrap();
    assert_eq!(
        choices[0].delta.reasoning_content.as_deref(),
        Some("First I will think about this problem carefully")
    );
    assert!(choices[0].delta.content.is_none());
}

#[test]
fn chat_response_deserializes_from_fixture() {
    let json = include_str!("fixtures/chat_response.json");
    let response: ChatResponse = serde_json::from_str(json).expect("deserialize");

    assert_eq!(response.id, "chatcmpl-abc123");
    assert_eq!(response.model, "deepseek-v4-flash");
    assert_eq!(response.choices.len(), 1);
    assert_eq!(response.choices[0].finish_reason.as_deref(), Some("stop"));
    assert_eq!(
        response.choices[0]
            .message
            .content
            .as_ref()
            .and_then(Content::as_text),
        Some("Hello! I'm DeepSeek, an AI assistant. How can I help you today?")
    );
    let usage = response.usage.as_ref().unwrap();
    assert_eq!(usage.prompt_tokens, 10);
    assert_eq!(usage.completion_tokens, 15);
    assert_eq!(usage.total_tokens, 25);
    assert_eq!(usage.prompt_cache_hit_tokens, 8);
    assert_eq!(usage.prompt_cache_miss_tokens, 2);
    assert_eq!(usage.prompt_cache_write_tokens, 10);
}

#[test]
fn stream_chunk_parses_sse_data_line() {
    let data = r#"{"id":"chatcmpl-xyz","object":"chat.completion.chunk","created":1716902400,"model":"deepseek-v4-flash","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}"#;
    let chunk: StreamChunk = serde_json::from_str(data).expect("parse");

    let choices = chunk.choices.as_ref().unwrap();
    assert_eq!(choices[0].delta.content.as_deref(), Some("Hello"));
}

#[test]
fn stream_chunk_parses_done_marker() {
    let data = r#"{"id":"chatcmpl-xyz","object":"chat.completion.chunk","created":1716902400,"model":"deepseek-v4-flash","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#;
    let chunk: StreamChunk = serde_json::from_str(data).expect("parse");

    let choices = chunk.choices.as_ref().unwrap();
    assert_eq!(choices[0].finish_reason.as_deref(), Some("stop"));
    assert!(choices[0].delta.content.is_none());
}

#[test]
fn stream_chunk_parses_final_chunk_with_usage() {
    // Final chunk with usage (DeepSeek sends cache stats in last delta before [DONE])
    let data = r#"{"id":"chatcmpl-xyz","object":"chat.completion.chunk","created":1716902400,"model":"deepseek-v4-flash","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":120,"completion_tokens":45,"total_tokens":165,"prompt_cache_hit_tokens":100,"prompt_cache_miss_tokens":20,"prompt_cache_write_tokens":120}}"#;
    let chunk: StreamChunk = serde_json::from_str(data).expect("parse");

    let usage = chunk.usage.as_ref().unwrap();
    assert_eq!(usage.prompt_tokens, 120);
    assert_eq!(usage.completion_tokens, 45);
    assert_eq!(usage.total_tokens, 165);
    assert_eq!(usage.prompt_cache_hit_tokens, 100);
    assert_eq!(usage.prompt_cache_miss_tokens, 20);
    assert_eq!(usage.prompt_cache_write_tokens, 120);
}

#[test]
fn usage_without_cache_fields_defaults_to_zero() {
    // Backward compat: older API responses or non-cached requests don't include cache fields
    let data = r#"{"prompt_tokens":10,"completion_tokens":15,"total_tokens":25}"#;
    let usage: Usage = serde_json::from_str(data).expect("parse");
    assert_eq!(usage.prompt_tokens, 10);
    assert_eq!(usage.completion_tokens, 15);
    assert_eq!(usage.total_tokens, 25);
    assert_eq!(usage.prompt_cache_hit_tokens, 0);
    assert_eq!(usage.prompt_cache_miss_tokens, 0);
    assert_eq!(usage.prompt_cache_write_tokens, 0);
}

#[test]
fn text_content_serializes_as_bare_string_byte_for_byte() {
    // Pins the old `Option<String>` wire shape: a text-only message
    // must still serialize its content as a bare JSON string, not a
    // wrapped object or a one-element array. This is the exact shape
    // `Option<String>` produced before `Content` existed.
    let msg = Message {
        role: Role::User,
        content: Some(Content::text("hello")),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    };
    let json = serde_json::to_string(&msg).expect("serialize");
    assert_eq!(json, r#"{"role":"user","content":"hello"}"#);
}

#[test]
fn absent_content_is_omitted_from_the_wire_exactly_as_before() {
    // The `None` case is real: an assistant message that only calls
    // tools has no content. It must stay entirely absent from the
    // wire, as `skip_serializing_if` already gave it.
    let msg = Message {
        role: Role::Assistant,
        content: None,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    };
    let json = serde_json::to_string(&msg).expect("serialize");
    assert_eq!(json, r#"{"role":"assistant"}"#);
}

#[test]
fn parts_content_serializes_to_openai_compatible_array() {
    let msg = Message {
        role: Role::User,
        content: Some(Content::Parts(vec![
            ContentPart::Text {
                text: "look at this".into(),
            },
            ContentPart::ImageUrl {
                url: "data:image/png;base64,AAA".into(),
            },
        ])),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    };
    let json = serde_json::to_string(&msg).expect("serialize");
    assert_eq!(
        json,
        r#"{"role":"user","content":[{"type":"text","text":"look at this"},{"type":"image_url","image_url":{"url":"data:image/png;base64,AAA"}}]}"#
    );
}

#[test]
fn text_content_round_trips_through_json() {
    let msg = Message {
        role: Role::User,
        content: Some(Content::text("round trip me")),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    };
    let json = serde_json::to_string(&msg).expect("serialize");
    let back: Message = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(
        back.content.as_ref().and_then(Content::as_text),
        Some("round trip me")
    );
}

#[test]
fn a_plain_string_content_field_deserializes_as_text() {
    // An old session file, saved before `Content` existed, has
    // `content` as a bare JSON string. It must still load.
    let json = r#"{"role":"user","content":"from an old session file"}"#;
    let msg: Message = serde_json::from_str(json).expect("deserialize");
    assert_eq!(
        msg.content.as_ref().and_then(Content::as_text),
        Some("from an old session file")
    );
}

#[test]
fn parts_content_round_trips_through_json() {
    let original = Content::Parts(vec![ContentPart::Text { text: "hi".into() }]);
    let json = serde_json::to_string(&original).expect("serialize");
    let back: Content = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, original);
}
