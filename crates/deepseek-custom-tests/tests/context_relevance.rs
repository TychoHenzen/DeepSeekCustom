//! Unit tests for `deepseek_custom::context::relevance` (`src/context/relevance.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use deepseek_custom::api::client::Provider;
use deepseek_custom::api::types::{FunctionCall, Message, Role, ToolCall};
use deepseek_custom::context::relevance::{build_index, parse_scores, scoring_model};

use serde_json::Value;

fn tool_call(name: &str) -> ToolCall {
    ToolCall {
        id: "call1".into(),
        call_type: "function".into(),
        function: Some(FunctionCall {
            name: Some(name.into()),
            arguments: Some("{}".into()),
        }),
        index: None,
    }
}

#[test]
fn build_index_emits_one_entry_per_message_with_ids_and_roles() {
    let messages = vec![
        Message::user("hi".into()),
        Message::assistant("hello".into()),
        Message::tool_result("call1".into(), "result".into()),
    ];
    let index = build_index(&messages);
    let parsed: Value = serde_json::from_str(&index).expect("valid JSON");
    let arr = parsed.as_array().expect("array");
    assert_eq!(arr.len(), 3);
    assert_eq!(arr[0]["id"], 0);
    assert_eq!(arr[0]["role"], "user");
    assert_eq!(arr[1]["id"], 1);
    assert_eq!(arr[1]["role"], "assistant");
    assert_eq!(arr[2]["id"], 2);
    assert_eq!(arr[2]["role"], "tool");
}

#[test]
fn build_index_truncates_long_preview_and_collapses_newlines() {
    let long_text = format!("line one\nline two\n{}", "x".repeat(200));
    let messages = vec![Message::user(long_text)];
    let index = build_index(&messages);
    let parsed: Value = serde_json::from_str(&index).expect("valid JSON");
    let preview = parsed[0]["preview"].as_str().expect("preview string");
    assert_eq!(preview.chars().count(), 100);
    assert!(!preview.contains('\n'));
    assert!(preview.starts_with("line one line two"));
}

#[test]
fn build_index_previews_tool_call_names_when_no_content() {
    let msg = Message {
        role: Role::Assistant,
        content: None,
        tool_calls: Some(vec![tool_call("bash"), tool_call("read")]),
        tool_call_id: None,
        reasoning_content: None,
    };
    let index = build_index(&[msg]);
    let parsed: Value = serde_json::from_str(&index).expect("valid JSON");
    assert_eq!(parsed[0]["preview"], "calls: bash, read");
}

#[test]
fn build_index_previews_empty_string_when_nothing_to_preview() {
    let msg = Message {
        role: Role::Assistant,
        content: None,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    };
    let index = build_index(&[msg]);
    let parsed: Value = serde_json::from_str(&index).expect("valid JSON");
    assert_eq!(parsed[0]["preview"], "");
}

#[test]
fn parse_scores_handles_clean_json_array() {
    let reply = r#"[{"id":0,"score":0.2},{"id":1,"score":0.9}]"#;
    let scores = parse_scores(reply, 2).expect("parses");
    assert_eq!(scores, vec![0.2, 0.9]);
}

#[test]
fn parse_scores_handles_markdown_code_fence() {
    let reply = "```json\n[{\"id\":0,\"score\":0.3}]\n```";
    let scores = parse_scores(reply, 1).expect("parses");
    assert_eq!(scores, vec![0.3]);
}

#[test]
fn parse_scores_handles_prose_around_array() {
    let reply = "Here are the scores: [{\"id\":0,\"score\":0.4}] hope that helps!";
    let scores = parse_scores(reply, 1).expect("parses");
    assert_eq!(scores, vec![0.4]);
}

#[test]
fn parse_scores_returns_none_on_malformed_json() {
    let reply = "[{\"id\":0,\"score\":]";
    assert!(parse_scores(reply, 1).is_none());
}

#[test]
fn parse_scores_returns_none_when_id_missing() {
    let reply = r#"[{"id":0,"score":0.5}]"#;
    assert!(parse_scores(reply, 2).is_none());
}

#[test]
fn parse_scores_returns_none_on_out_of_range_id() {
    let reply = r#"[{"id":0,"score":0.5},{"id":5,"score":0.5}]"#;
    assert!(parse_scores(reply, 2).is_none());
}

#[test]
fn parse_scores_returns_none_on_duplicate_id() {
    let reply = r#"[{"id":0,"score":0.5},{"id":0,"score":0.9}]"#;
    assert!(parse_scores(reply, 1).is_none());
}

#[test]
fn parse_scores_returns_none_on_non_finite_score() {
    // JSON has no NaN literal. A literal that big is valid JSON syntax
    // but overflows to infinity once read as an f64. That is the only
    // way a reply reaches the non-finite check, so it is what this
    // test uses.
    let inf_reply = r#"[{"id":0,"score":1e400}]"#;
    assert!(parse_scores(inf_reply, 1).is_none());
}

#[test]
fn parse_scores_clamps_out_of_range_scores() {
    let reply = r#"[{"id":0,"score":1.5},{"id":1,"score":-0.5}]"#;
    let scores = parse_scores(reply, 2).expect("parses");
    assert_eq!(scores, vec![1.0, 0.0]);
}

#[test]
fn scoring_model_uses_flash_for_deepseek() {
    let model = scoring_model(Provider::DeepSeek, "deepseek-v4-pro");
    assert_eq!(model, "deepseek-v4-flash");
}

#[test]
fn scoring_model_uses_the_conversation_model_for_ollama() {
    let model = scoring_model(Provider::Ollama, "qwen2.5-coder:7b-instruct-q4_K_M");
    assert_eq!(model, "qwen2.5-coder:7b-instruct-q4_K_M");
}
