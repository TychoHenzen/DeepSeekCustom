//! Unit tests for `deepseek_custom::api::client` (`src/api/client.rs`),
//! moved out of the production module as part of the two-crate workspace split.
//! Key resolution tests moved to `api_key.rs` when `client.rs` was split.

use deepseek_custom::api::client::ApiClient;
use deepseek_custom::api::provider::Provider;
use deepseek_custom::api::types::{ChatRequest, ToolChoice};
use deepseek_custom::effort::Effort;

fn sample_request(effort: Option<Effort>) -> ChatRequest {
    ChatRequest {
        model: "test-model".into(),
        messages: vec![],
        tools: None,
        tool_choice: Some(ToolChoice::Auto),
        stream: false,
        temperature: Some(0.7),
        max_tokens: Some(1024),
        thinking: None,
        thinking_mode: None,
        reasoning_effort: None,
        effort,
    }
}

#[test]
fn deepseek_sets_non_thinking_for_effort_none() {
    let client = ApiClient::new(Provider::DeepSeek, "sk-test".into(), None);
    let req = sample_request(Some(Effort::None));

    let prepared = client.prepare_request_for_test(&req);

    assert_eq!(prepared.thinking_mode.as_deref(), Some("non-thinking"));
    assert!(prepared.tool_choice.is_some());
    assert_eq!(prepared.reasoning_effort, None);
}

#[test]
fn deepseek_collapses_low_medium_high_into_thinking() {
    let client = ApiClient::new(Provider::DeepSeek, "sk-test".into(), None);
    for level in [Effort::Low, Effort::Medium, Effort::High] {
        let req = sample_request(Some(level));
        let prepared = client.prepare_request_for_test(&req);
        assert_eq!(
            prepared.thinking_mode.as_deref(),
            Some("thinking"),
            "level {level:?} should map to \"thinking\""
        );
    }
}

#[test]
fn deepseek_sets_thinking_max_for_effort_max() {
    let client = ApiClient::new(Provider::DeepSeek, "sk-test".into(), None);
    let req = sample_request(Some(Effort::Max));

    let prepared = client.prepare_request_for_test(&req);

    assert_eq!(prepared.thinking_mode.as_deref(), Some("thinking_max"));
}

#[test]
fn deepseek_with_no_effort_leaves_thinking_mode_untouched() {
    let client = ApiClient::new(Provider::DeepSeek, "sk-test".into(), None);
    let mut req = sample_request(None);
    req.thinking_mode = Some("thinking".to_string());

    let prepared = client.prepare_request_for_test(&req);

    assert_eq!(prepared.thinking_mode.as_deref(), Some("thinking"));
}

#[test]
fn ollama_clears_thinking_mode_and_tool_choice() {
    let client = ApiClient::new(Provider::Ollama, "sk-test".into(), None);
    let req = sample_request(Some(Effort::High));

    let prepared = client.prepare_request_for_test(&req);

    assert_eq!(prepared.thinking_mode, None);
    assert!(prepared.tool_choice.is_none());
}

#[test]
fn ollama_maps_every_level_one_to_one() {
    let client = ApiClient::new(Provider::Ollama, "sk-test".into(), None);
    let cases = [
        (Effort::None, "none"),
        (Effort::Low, "low"),
        (Effort::Medium, "medium"),
        (Effort::High, "high"),
        (Effort::Max, "max"),
    ];
    for (level, expected) in cases {
        let req = sample_request(Some(level));
        let prepared = client.prepare_request_for_test(&req);
        assert_eq!(
            prepared.reasoning_effort.as_deref(),
            Some(expected),
            "level {level:?} should map to {expected:?}"
        );
    }
}

#[test]
fn ollama_with_no_effort_leaves_reasoning_effort_alone() {
    let client = ApiClient::new(Provider::Ollama, "sk-test".into(), None);
    let mut req = sample_request(None);
    req.reasoning_effort = Some("low".to_string());

    let prepared = client.prepare_request_for_test(&req);

    assert_eq!(prepared.reasoning_effort.as_deref(), Some("low"));
}
