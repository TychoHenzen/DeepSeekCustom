//! Unit tests for `deepseek_custom::api::key` (`src/api/key.rs`),
//! moved out of `api_client.rs` when `client.rs` was split into
//! `client.rs`, `provider.rs`, `key.rs`, and `stream.rs`.

use deepseek_custom::api::key::{key_from_backends_json, parse_backends_json, resolve_api_key};
use deepseek_custom::api::provider::Provider;
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
