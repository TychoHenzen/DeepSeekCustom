//! Unit tests for `deepseek_custom::api::client`, moved out of the
//! production module as part of the two-crate workspace split.

use deepseek_custom::api::client::{
    ApiClient, Provider, key_from_backends_json, parse_backends_json, resolve_api_key,
};
use deepseek_custom::api::types::{ChatRequest, ToolChoice};
use deepseek_custom::effort::Effort;
use deepseek_custom::error::HarnessError;

#[test]
fn missing_api_key_returns_clear_error() {
    // Skip if any key source is available in the environment
    if std::env::var("DEEPSEEK_API_KEY").is_ok() {
        return;
    }
    if std::env::var("ANTHROPIC_AUTH_TOKEN").is_ok() {
        return;
    }
    if key_from_backends_json().is_some() {
        return;
    }

    let tmp = std::env::temp_dir().join("deepseek_test_missing_key");
    let _ = std::fs::create_dir_all(&tmp);

    let result = resolve_api_key(Provider::DeepSeek, &tmp);
    assert!(result.is_err());
    let err = result.unwrap_err();
    match err {
        HarnessError::Config(ref msg) => {
            assert!(msg.contains("DEEPSEEK_API_KEY"));
            assert!(msg.contains("platform.deepseek.com"));
        }
        _ => panic!("expected Config error, got {err:?}"),
    }

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn ollama_key_resolves_to_placeholder_without_touching_env_or_disk() {
    let tmp = std::env::temp_dir().join("deepseek_test_ollama_key_empty_root");
    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::create_dir_all(&tmp);

    let result = resolve_api_key(Provider::Ollama, &tmp);

    assert_eq!(result.unwrap(), "ollama");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn backends_json_prefers_the_default_backend() {
    let contents = r#"{
        "default": "deepseek-home",
        "backends": {
            "windows": {"label": "Anthropic"},
            "deepseek-home": {"apiKey": "sk-deep"},
            "other": {"apiKey": "sk-other"}
        }
    }"#;
    assert_eq!(parse_backends_json(contents), Some("sk-deep".to_string()));
}

#[test]
fn backends_json_falls_back_to_any_backend_with_a_key() {
    let contents = r#"{
        "default": "windows",
        "backends": {
            "windows": {"label": "Anthropic"},
            "deepseek-home": {"apiKey": "sk-deep"}
        }
    }"#;
    assert_eq!(parse_backends_json(contents), Some("sk-deep".to_string()));
}

#[test]
fn backends_json_without_any_key_yields_none() {
    let contents = r#"{"default": "windows", "backends": {"windows": {"apiKey": ""}}}"#;
    assert_eq!(parse_backends_json(contents), None);
}

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
