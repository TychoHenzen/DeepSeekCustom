//! Unit tests for `deepseek_custom::config::settings` (`src/config/settings.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use std::collections::HashMap;
use std::path::Path;

use deepseek_custom::config::settings::{
    ApiProvider, AutopilotConfig, BackendConfig, PermissionsConfig, Settings, TriggerMode,
    VoiceConfig,
};
use deepseek_custom::effort::Effort;

#[test]
fn load_nonexistent_file_returns_defaults() {
    let result = Settings::load(Path::new("/nonexistent/path/xyz"));
    assert!(result.is_ok());
    let s = result.unwrap();
    assert!(s.api_key.is_none());
}

#[test]
fn merge_overwrites_some_fields() {
    let mut base = Settings::default();
    let other = Settings {
        api_key: Some("sk-abc".into()),
        ..Default::default()
    };
    base.merge_for_test(other);
    assert_eq!(base.api_key.unwrap(), "sk-abc");
}

#[test]
fn voice_defaults_when_key_absent() {
    let s: Settings = serde_json::from_str("{}").unwrap();
    assert!(!s.voice_enabled());
    assert!(!s.voice_stt_enabled());
    assert!(!s.voice_tts_enabled());
    assert_eq!(s.voice_trigger_mode(), TriggerMode::PushToTalk);
    assert_eq!(s.voice_wake_phrase(), "hey deepseek");
    assert_eq!(s.voice_tts_speed(), 1.0);
    assert!(s.voice_stt_model_path().is_none());
    assert!(s.voice_tts_model_path().is_none());
    assert!(s.voice_tts_voices_path().is_none());
    assert_eq!(s.voice_tts_voice(), "af_heart");
}

#[test]
fn voice_full_block_deserializes() {
    let json = r#"{
        "voice": {
            "enabled": true,
            "stt_enabled": true,
            "tts_enabled": true,
            "stt_model_path": "C:/models/ggml-base.bin",
            "tts_model_path": "C:/models/model.onnx",
            "tts_voices_path": "C:/voices",
            "trigger_mode": "wake_word",
            "wake_phrase": "hey computer",
            "tts_voice": "am_michael",
            "tts_speed": 1.3
        }
    }"#;
    let s: Settings = serde_json::from_str(json).unwrap();
    assert!(s.voice_enabled());
    assert!(s.voice_stt_enabled());
    assert!(s.voice_tts_enabled());
    assert_eq!(s.voice_stt_model_path().unwrap(), "C:/models/ggml-base.bin");
    assert_eq!(s.voice_tts_model_path().unwrap(), "C:/models/model.onnx");
    assert_eq!(s.voice_tts_voices_path().unwrap(), "C:/voices");
    assert_eq!(s.voice_trigger_mode(), TriggerMode::WakeWord);
    assert_eq!(s.voice_wake_phrase(), "hey computer");
    assert_eq!(s.voice_tts_voice(), "am_michael");
    assert_eq!(s.voice_tts_speed(), 1.3);
}

#[test]
fn voice_tts_speed_clamps_out_of_range_values() {
    let json = r#"{"voice": {"tts_speed": 9.0}}"#;
    let s: Settings = serde_json::from_str(json).unwrap();
    assert_eq!(s.voice_tts_speed(), 2.0);

    let json = r#"{"voice": {"tts_speed": 0.01}}"#;
    let s: Settings = serde_json::from_str(json).unwrap();
    assert_eq!(s.voice_tts_speed(), 0.5);
}

#[test]
fn trigger_mode_parses_push_to_talk() {
    let json = r#"{"voice": {"trigger_mode": "push_to_talk"}}"#;
    let s: Settings = serde_json::from_str(json).unwrap();
    assert_eq!(s.voice_trigger_mode(), TriggerMode::PushToTalk);
}

#[test]
fn trigger_mode_parses_wake_word() {
    let json = r#"{"voice": {"trigger_mode": "wake_word"}}"#;
    let s: Settings = serde_json::from_str(json).unwrap();
    assert_eq!(s.voice_trigger_mode(), TriggerMode::WakeWord);
}

/// Create a uniquely named directory under the system temp dir.
fn unique_temp_dir(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("dsc-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn save_then_load_round_trips_values() {
    let dir = unique_temp_dir("settings-roundtrip");

    let original = Settings {
        api_key: Some("sk-round-trip".into()),
        permissions: Some(PermissionsConfig {
            allow: Some(vec!["Bash".into(), "Read".into()]),
            deny: None,
        }),
        hooks: None,
        effort: Some(Effort::High),
        voice: Some(VoiceConfig {
            enabled: true,
            stt_enabled: true,
            tts_enabled: true,
            stt_model_path: Some("C:/models/ggml-base.bin".into()),
            tts_model_path: Some("C:/models/model.onnx".into()),
            tts_voices_path: Some("C:/voices".into()),
            trigger_mode: TriggerMode::WakeWord,
            wake_phrase: Some("hey computer".into()),
            tts_voice: Some("am_michael".into()),
            tts_speed: Some(1.3),
        }),
        autopilot: Some(AutopilotConfig {
            iterations: Some(10),
            policy_path: Some("custom-policy.md".into()),
            answerer_model: Some("deepseek-v4-pro".into()),
            task: Some("do the thing".into()),
        }),
        context_budget: Some(150_000),
        show_raw_output: Some(true),
        backends: None,
        default_backend: None,
        session_turn_cap: None,
        send_message_call_cap: None,
        subagent_max_depth: Some(3),
        working_dir: None,
        mcp: None,
    };

    original.save(&dir).unwrap();
    assert!(dir.join("settings.json").exists());

    let loaded = Settings::load(&dir).unwrap();

    assert_eq!(loaded.api_key.as_deref(), Some("sk-round-trip"));
    let perms = loaded.permissions.as_ref().unwrap();
    assert_eq!(
        perms.allow.as_ref().unwrap(),
        &vec!["Bash".to_string(), "Read".to_string()]
    );
    assert!(perms.deny.is_none());
    assert_eq!(loaded.effort(), Effort::High);
    assert!(loaded.voice_enabled());
    assert!(loaded.voice_stt_enabled());
    assert!(loaded.voice_tts_enabled());
    assert_eq!(
        loaded.voice_stt_model_path().unwrap(),
        "C:/models/ggml-base.bin"
    );
    assert_eq!(
        loaded.voice_tts_model_path().unwrap(),
        "C:/models/model.onnx"
    );
    assert_eq!(loaded.voice_tts_voices_path().unwrap(), "C:/voices");
    assert_eq!(loaded.voice_trigger_mode(), TriggerMode::WakeWord);
    assert_eq!(loaded.voice_wake_phrase(), "hey computer");
    assert_eq!(loaded.voice_tts_voice(), "am_michael");
    assert_eq!(loaded.voice_tts_speed(), 1.3);
    assert_eq!(loaded.autopilot_iterations(), 10);
    assert_eq!(loaded.autopilot_policy_path().unwrap(), "custom-policy.md");
    assert_eq!(loaded.autopilot_answerer_model(), "deepseek-v4-pro");
    assert_eq!(loaded.autopilot_task().unwrap(), "do the thing");
    assert_eq!(loaded.subagent_max_depth(), 3);

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn autopilot_defaults_when_absent() {
    let s: Settings = serde_json::from_str("{}").unwrap();
    assert_eq!(s.autopilot_iterations(), 5);
    assert!(s.autopilot_policy_path().is_none());
    assert_eq!(s.autopilot_answerer_model(), "deepseek-v4-flash");
    assert!(s.autopilot_task().is_none());
}

#[test]
fn autopilot_reads_set_values() {
    let json = r#"{
        "autopilot": {
            "iterations": 3,
            "policy_path": "policies/autopilot.md",
            "answerer_model": "deepseek-v4-pro",
            "task": "fix the build"
        }
    }"#;
    let s: Settings = serde_json::from_str(json).unwrap();
    assert_eq!(s.autopilot_iterations(), 3);
    assert_eq!(s.autopilot_policy_path().unwrap(), "policies/autopilot.md");
    assert_eq!(s.autopilot_answerer_model(), "deepseek-v4-pro");
    assert_eq!(s.autopilot_task().unwrap(), "fix the build");
}

#[test]
fn autopilot_mut_creates_block_with_defaults() {
    let mut s = Settings::default();
    assert!(s.autopilot.is_none());
    let block = s.autopilot_mut();
    assert!(block.iterations.is_none());
    block.iterations = Some(7);
    assert_eq!(s.autopilot_iterations(), 7);
}

#[test]
fn autopilot_absent_settings_serialize_without_key() {
    let s = Settings::default();
    let json = serde_json::to_string(&s).unwrap();
    assert!(!json.contains("autopilot"));
}

#[test]
fn save_omits_none_fields() {
    let dir = unique_temp_dir("settings-omit");

    let s = Settings {
        api_key: None,
        permissions: None,
        hooks: None,
        effort: None,
        voice: None,
        autopilot: None,
        context_budget: None,
        show_raw_output: None,
        backends: None,
        default_backend: None,
        subagent_max_depth: None,
        session_turn_cap: None,
        send_message_call_cap: None,
        working_dir: None,
        mcp: None,
    };
    s.save(&dir).unwrap();
    let text = std::fs::read_to_string(dir.join("settings.json")).unwrap();

    assert!(!text.contains("api_key"));
    assert!(!text.contains("context_budget"));
    assert!(!text.contains("show_raw_output"));
    assert!(!text.contains("subagent_max_depth"));
    assert!(!text.contains("null"));

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn context_budget_defaults_when_absent() {
    let s: Settings = serde_json::from_str("{}").unwrap();
    assert_eq!(s.context_budget(), 100_000);
}

#[test]
fn context_budget_passes_through_in_range_value() {
    let s: Settings = serde_json::from_str(r#"{"context_budget": 64000}"#).unwrap();
    assert_eq!(s.context_budget(), 64_000);
}

#[test]
fn context_budget_clamps_low_value() {
    let s: Settings = serde_json::from_str(r#"{"context_budget": 1000}"#).unwrap();
    assert_eq!(s.context_budget(), 32_000);
}

#[test]
fn context_budget_clamps_high_value() {
    let s: Settings = serde_json::from_str(r#"{"context_budget": 999999}"#).unwrap();
    assert_eq!(s.context_budget(), 200_000);
}

#[test]
fn show_raw_output_defaults_to_false() {
    let s: Settings = serde_json::from_str("{}").unwrap();
    assert!(!s.show_raw_output());
}

#[test]
fn show_raw_output_reads_set_value() {
    let s: Settings = serde_json::from_str(r#"{"show_raw_output": true}"#).unwrap();
    assert!(s.show_raw_output());
}

#[test]
fn effort_defaults_to_none() {
    let s: Settings = serde_json::from_str("{}").unwrap();
    assert_eq!(s.effort(), Effort::None);
}

#[test]
fn effort_reads_set_value() {
    let s: Settings = serde_json::from_str(r#"{"effort": "high"}"#).unwrap();
    assert_eq!(s.effort(), Effort::High);
}

/// A `settings.json` written before this control existed may still
/// carry the old `thinking` block. Loading it must not fail, and it
/// must not resurrect the old boolean behaviour: the unknown field is
/// dropped, and `effort()` reports its own default, `Effort::None`,
/// same as a `settings.json` that never had a `thinking` block at all.
#[test]
fn a_settings_file_with_the_old_thinking_block_loads_and_effort_defaults_to_none() {
    let s: Settings =
        serde_json::from_str(r#"{"thinking": {"enabled": true, "effort": "high"}}"#).unwrap();
    assert_eq!(s.effort(), Effort::None);
}

#[test]
fn new_panel_fields_round_trip_through_a_file() {
    let dir = unique_temp_dir("settings-panel-fields");

    let original = Settings {
        context_budget: Some(150_000),
        show_raw_output: Some(true),
        effort: Some(Effort::Max),
        ..Default::default()
    };
    original.save(&dir).unwrap();

    let loaded = Settings::load(&dir).unwrap();
    assert_eq!(loaded.context_budget(), 150_000);
    assert!(loaded.show_raw_output());
    assert_eq!(loaded.effort(), Effort::Max);

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn trigger_mode_unknown_value_falls_back() {
    let json = r#"{"voice": {"trigger_mode": "bogus_mode"}}"#;
    let s: Settings = serde_json::from_str(json).unwrap();
    assert_eq!(s.voice_trigger_mode(), TriggerMode::PushToTalk);
}

#[test]
fn api_backend_minimal_fields_deserialize() {
    let json = r#"{"kind": "api", "provider": "deepseek", "model": "deepseek-v4-pro"}"#;
    let b: BackendConfig = serde_json::from_str(json).unwrap();
    match b {
        BackendConfig::Api {
            provider,
            model,
            base_url,
            api_key,
            models,
        } => {
            assert_eq!(provider, ApiProvider::DeepSeek);
            assert_eq!(model, "deepseek-v4-pro");
            assert!(base_url.is_none());
            assert!(api_key.is_none());
            assert!(models.is_none());
        }
        BackendConfig::ClaudeCli { .. } => panic!("expected Api variant"),
    }
}

#[test]
fn api_backend_full_fields_round_trip() {
    let original = BackendConfig::Api {
        provider: ApiProvider::Ollama,
        model: "qwen2.5-coder:7b-instruct-q4_K_M".into(),
        base_url: Some("http://localhost:11434/v1".into()),
        api_key: Some("sk-local".into()),
        models: None,
    };
    let json = serde_json::to_string(&original).unwrap();
    let loaded: BackendConfig = serde_json::from_str(&json).unwrap();
    match loaded {
        BackendConfig::Api {
            provider,
            model,
            base_url,
            api_key,
            models,
        } => {
            assert_eq!(provider, ApiProvider::Ollama);
            assert_eq!(model, "qwen2.5-coder:7b-instruct-q4_K_M");
            assert_eq!(base_url.as_deref(), Some("http://localhost:11434/v1"));
            assert_eq!(api_key.as_deref(), Some("sk-local"));
            assert!(models.is_none());
        }
        BackendConfig::ClaudeCli { .. } => panic!("expected Api variant"),
    }
}

#[test]
fn claude_cli_backend_round_trips() {
    let mut env = HashMap::new();
    env.insert("FOO".to_string(), "bar".to_string());
    env.insert("BAZ".to_string(), "qux".to_string());
    let original = BackendConfig::ClaudeCli {
        model: "opus".into(),
        permission_mode: Some("bypassPermissions".into()),
        env: Some(env),
        models: None,
    };
    let json = serde_json::to_string(&original).unwrap();
    let loaded: BackendConfig = serde_json::from_str(&json).unwrap();
    match loaded {
        BackendConfig::ClaudeCli {
            model,
            permission_mode,
            env,
            models,
        } => {
            assert_eq!(model, "opus");
            assert_eq!(permission_mode.as_deref(), Some("bypassPermissions"));
            let env = env.unwrap();
            assert_eq!(env.get("FOO").map(String::as_str), Some("bar"));
            assert_eq!(env.get("BAZ").map(String::as_str), Some("qux"));
            assert!(models.is_none());
        }
        BackendConfig::Api { .. } => panic!("expected ClaudeCli variant"),
    }
}

#[test]
fn api_backend_omits_absent_optional_fields() {
    let backend = BackendConfig::Api {
        provider: ApiProvider::DeepSeek,
        model: "deepseek-v4-pro".into(),
        base_url: None,
        api_key: None,
        models: None,
    };
    let json = serde_json::to_string(&backend).unwrap();
    assert!(!json.contains("base_url"));
    assert!(!json.contains("api_key"));
    assert!(!json.contains("models"));
}

#[test]
fn models_override_round_trips_on_api_backend() {
    let original = BackendConfig::Api {
        provider: ApiProvider::DeepSeek,
        model: "deepseek-v4-pro".into(),
        base_url: None,
        api_key: None,
        models: Some(vec!["deepseek-v4-pro".into(), "deepseek-v4-flash".into()]),
    };
    let json = serde_json::to_string(&original).unwrap();
    let loaded: BackendConfig = serde_json::from_str(&json).unwrap();
    match loaded {
        BackendConfig::Api { models, .. } => {
            assert_eq!(
                models,
                Some(vec![
                    "deepseek-v4-pro".to_string(),
                    "deepseek-v4-flash".to_string()
                ])
            );
        }
        BackendConfig::ClaudeCli { .. } => panic!("expected Api variant"),
    }
}

#[test]
fn models_override_round_trips_on_claude_cli_backend() {
    let original = BackendConfig::ClaudeCli {
        model: "opus".into(),
        permission_mode: None,
        env: None,
        models: Some(vec!["opus".into(), "sonnet".into()]),
    };
    let json = serde_json::to_string(&original).unwrap();
    let loaded: BackendConfig = serde_json::from_str(&json).unwrap();
    match loaded {
        BackendConfig::ClaudeCli { models, .. } => {
            assert_eq!(models, Some(vec!["opus".to_string(), "sonnet".to_string()]));
        }
        BackendConfig::Api { .. } => panic!("expected ClaudeCli variant"),
    }
}

#[test]
fn models_field_absent_from_json_when_none() {
    let api = BackendConfig::Api {
        provider: ApiProvider::DeepSeek,
        model: "deepseek-v4-pro".into(),
        base_url: None,
        api_key: None,
        models: None,
    };
    assert!(!serde_json::to_string(&api).unwrap().contains("models"));

    let claude = BackendConfig::ClaudeCli {
        model: "opus".into(),
        permission_mode: None,
        env: None,
        models: None,
    };
    assert!(!serde_json::to_string(&claude).unwrap().contains("models"));
}

#[test]
fn resolve_backend_finds_by_name_and_misses_unknown() {
    let mut backends = HashMap::new();
    backends.insert(
        "deepseek".to_string(),
        BackendConfig::Api {
            provider: ApiProvider::DeepSeek,
            model: "deepseek-v4-pro".into(),
            base_url: None,
            api_key: None,
            models: None,
        },
    );
    let s = Settings {
        backends: Some(backends),
        default_backend: Some("deepseek".into()),
        ..Default::default()
    };

    match s.resolve_backend("deepseek") {
        Some(BackendConfig::Api { provider, .. }) => {
            assert_eq!(*provider, ApiProvider::DeepSeek);
        }
        _ => panic!("expected to resolve deepseek backend"),
    }
    assert!(s.resolve_backend("nonexistent").is_none());
    assert_eq!(s.default_backend(), Some("deepseek"));
}

#[test]
fn subagent_max_depth_defaults_to_two() {
    let s: Settings = serde_json::from_str("{}").unwrap();
    assert_eq!(s.subagent_max_depth(), 2);
}

#[test]
fn subagent_max_depth_round_trips_through_serialize_and_deserialize() {
    let s = Settings {
        subagent_max_depth: Some(4),
        ..Default::default()
    };
    let json = serde_json::to_string(&s).unwrap();
    assert!(json.contains("\"subagent_max_depth\":4"));

    let loaded: Settings = serde_json::from_str(&json).unwrap();
    assert_eq!(loaded.subagent_max_depth(), 4);
}

#[test]
fn subagent_max_depth_absent_settings_serialize_without_key() {
    let s = Settings::default();
    let json = serde_json::to_string(&s).unwrap();
    assert!(!json.contains("subagent_max_depth"));
}

#[test]
fn subagent_max_depth_takes_part_in_merge() {
    let mut base = Settings::default();
    assert_eq!(base.subagent_max_depth(), 2);

    let other = Settings {
        subagent_max_depth: Some(5),
        ..Default::default()
    };
    base.merge_for_test(other);
    assert_eq!(base.subagent_max_depth(), 5);
}

/// The repo `settings.json` is also the live settings file: the GUI
/// rewrites it whenever a control changes. So this test checks the
/// shape it must keep, not the choices a user is free to make. Asserting
/// an exact `default_backend` here would fail the suite for anyone who
/// touched the backend picker.
#[test]
fn repo_settings_json_parses_with_three_backends() {
    // The crate now sits two levels under the repo root
    // (crates/deepseek-custom), so a future move of the crate needs to
    // update this join count.
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let contents = std::fs::read_to_string(repo_root.join("settings.json")).unwrap();
    let s: Settings = serde_json::from_str(&contents).unwrap();

    let backends = s.backends().unwrap();
    assert_eq!(backends.len(), 3);

    let selected = s.default_backend().expect("default_backend must be set");
    assert!(
        backends.contains_key(selected),
        "default_backend {selected} names no configured entry"
    );

    match backends.get("deepseek").unwrap() {
        BackendConfig::Api {
            provider, model, ..
        } => {
            assert_eq!(*provider, ApiProvider::DeepSeek);
            assert!(!model.is_empty());
        }
        _ => panic!("expected deepseek to be an Api backend"),
    }

    match backends.get("ollama").unwrap() {
        BackendConfig::Api {
            provider, model, ..
        } => {
            assert_eq!(*provider, ApiProvider::Ollama);
            assert!(!model.is_empty());
        }
        _ => panic!("expected ollama to be an Api backend"),
    }

    match backends.get("claude").unwrap() {
        BackendConfig::ClaudeCli { model, .. } => {
            assert!(!model.is_empty());
        }
        _ => panic!("expected claude to be a ClaudeCli backend"),
    }
}

#[test]
fn provider_and_kind_wire_tags_round_trip() {
    let deepseek_json = serde_json::to_string(&ApiProvider::DeepSeek).unwrap();
    assert_eq!(deepseek_json, "\"deepseek\"");
    let ollama_json = serde_json::to_string(&ApiProvider::Ollama).unwrap();
    assert_eq!(ollama_json, "\"ollama\"");
    assert_eq!(
        serde_json::from_str::<ApiProvider>("\"deepseek\"").unwrap(),
        ApiProvider::DeepSeek
    );
    assert_eq!(
        serde_json::from_str::<ApiProvider>("\"ollama\"").unwrap(),
        ApiProvider::Ollama
    );

    let api_backend = BackendConfig::Api {
        provider: ApiProvider::DeepSeek,
        model: "deepseek-v4-pro".into(),
        base_url: None,
        api_key: None,
        models: None,
    };
    let api_json = serde_json::to_string(&api_backend).unwrap();
    assert!(api_json.contains("\"kind\":\"api\""));

    let claude_backend = BackendConfig::ClaudeCli {
        model: "opus".into(),
        permission_mode: None,
        env: None,
        models: None,
    };
    let claude_json = serde_json::to_string(&claude_backend).unwrap();
    assert!(claude_json.contains("\"kind\":\"claude_cli\""));
}
