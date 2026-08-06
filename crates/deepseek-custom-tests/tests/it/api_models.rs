//! Unit tests for `deepseek_custom::api::models`, moved out of the
//! production module as part of the two-crate workspace split.

use std::collections::HashMap;

use deepseek_custom::api::models::{
    apply_fallback, entry_models_override, list_models, ollama_tags_url, parse_ollama_tags,
};
use deepseek_custom::config::settings::{ApiProvider, BackendConfig};

fn ollama_entry(base_url: Option<&str>, models: Option<Vec<String>>) -> BackendConfig {
    BackendConfig::Api {
        provider: ApiProvider::Ollama,
        model: "qwen2.5:1.5b".to_string(),
        base_url: base_url.map(|s| s.to_string()),
        api_key: None,
        models,
    }
}

fn deepseek_entry(models: Option<Vec<String>>) -> BackendConfig {
    BackendConfig::Api {
        provider: ApiProvider::DeepSeek,
        model: "deepseek-v4-pro".to_string(),
        base_url: None,
        api_key: None,
        models,
    }
}

fn claude_cli_entry(models: Option<Vec<String>>) -> BackendConfig {
    BackendConfig::ClaudeCli {
        model: "opus".to_string(),
        permission_mode: None,
        env: None,
        models,
    }
}

#[test]
fn parse_ollama_tags_returns_names_in_order() {
    let body = r#"{"models":[{"name":"qwen2.5:1.5b"},{"name":"qwen2.5-coder:7b-instruct-q4_K_M"}]}"#;
    let names = parse_ollama_tags(body);
    assert_eq!(
        names,
        vec![
            "qwen2.5:1.5b".to_string(),
            "qwen2.5-coder:7b-instruct-q4_K_M".to_string()
        ]
    );
}

#[test]
fn parse_ollama_tags_malformed_json_yields_empty() {
    assert!(parse_ollama_tags("not json at all").is_empty());
}

#[test]
fn parse_ollama_tags_empty_models_array_yields_empty() {
    assert!(parse_ollama_tags(r#"{"models":[]}"#).is_empty());
}

#[test]
fn parse_ollama_tags_missing_models_key_yields_empty() {
    assert!(parse_ollama_tags(r#"{"other":1}"#).is_empty());
}

#[tokio::test]
async fn explicit_override_wins_for_api_variant() {
    let entry = ollama_entry(None, Some(vec!["custom-model".to_string()]));
    let models = list_models(&entry).await;
    assert_eq!(models, vec!["custom-model".to_string()]);
}

#[tokio::test]
async fn explicit_override_wins_for_claude_cli_variant() {
    let entry = claude_cli_entry(Some(vec!["custom-alias".to_string()]));
    let models = list_models(&entry).await;
    assert_eq!(models, vec!["custom-alias".to_string()]);
}

#[tokio::test]
async fn deepseek_entry_with_no_override_returns_known_models() {
    let entry = deepseek_entry(None);
    let models = list_models(&entry).await;
    assert_eq!(
        models,
        vec!["deepseek-v4-flash".to_string(), "deepseek-v4-pro".to_string()]
    );
}

#[tokio::test]
async fn claude_cli_entry_with_no_override_returns_known_aliases() {
    let entry = claude_cli_entry(None);
    let models = list_models(&entry).await;
    assert_eq!(
        models,
        vec![
            "opus".to_string(),
            "sonnet".to_string(),
            "haiku".to_string(),
            "fable".to_string()
        ]
    );
}

#[test]
fn fallback_yields_declared_model_when_discovery_is_empty() {
    let result = apply_fallback(Vec::new(), "declared-model");
    assert_eq!(result, vec!["declared-model".to_string()]);
}

#[test]
fn fallback_keeps_discovered_list_when_non_empty() {
    let result = apply_fallback(vec!["a".to_string(), "b".to_string()], "declared-model");
    assert_eq!(result, vec!["a".to_string(), "b".to_string()]);
}

#[test]
fn host_derivation_strips_trailing_v1() {
    assert_eq!(
        ollama_tags_url(Some("http://localhost:11434/v1")),
        "http://localhost:11434/api/tags"
    );
}

#[test]
fn host_derivation_preserves_custom_host() {
    assert_eq!(
        ollama_tags_url(Some("http://my-ollama-box:9999")),
        "http://my-ollama-box:9999/api/tags"
    );
}

#[test]
fn host_derivation_falls_back_to_default_when_absent() {
    assert_eq!(ollama_tags_url(None), "http://localhost:11434/api/tags");
}

#[test]
fn entry_models_override_reads_both_variants() {
    let api = ollama_entry(None, Some(vec!["x".to_string()]));
    assert_eq!(entry_models_override(&api), Some(vec!["x".to_string()]));

    let cli = claude_cli_entry(Some(vec!["y".to_string()]));
    assert_eq!(entry_models_override(&cli), Some(vec!["y".to_string()]));

    let none_env: Option<HashMap<String, String>> = None;
    let cli_none = BackendConfig::ClaudeCli {
        model: "opus".to_string(),
        permission_mode: None,
        env: none_env,
        models: None,
    };
    assert_eq!(entry_models_override(&cli_none), None);
}
