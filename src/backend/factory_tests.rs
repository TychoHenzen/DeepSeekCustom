//! Tests for `src/backend/factory.rs`. Split into its own file, loaded via
//! `#[path]`, to keep `factory.rs` under the file-length ratchet. `super::*`
//! reaches every private item defined there: `ResolvedBackend`,
//! `resolve_active_backend`, and `BackendFactory` itself.

use super::*;
use std::collections::HashMap;

fn settings_with_backends(
    default_backend: Option<&str>,
    backends: HashMap<String, BackendConfig>,
) -> Settings {
    let mut settings = Settings::default();
    settings.default_backend = default_backend.map(|s| s.to_string());
    settings.backends = Some(backends);
    settings
}

#[test]
fn resolves_valid_api_entry() {
    let mut backends = HashMap::new();
    backends.insert(
        "ollama".to_string(),
        BackendConfig::Api {
            provider: ApiProvider::Ollama,
            model: "qwen2.5-coder:7b-instruct-q4_K_M".to_string(),
            base_url: Some("http://localhost:11434/v1".to_string()),
            api_key: None,
            models: None,
        },
    );
    let settings = settings_with_backends(Some("ollama"), backends);

    let resolved = resolve_active_backend(&settings, Path::new(".")).expect("should resolve");

    match resolved {
        ResolvedBackend::Api {
            name,
            provider,
            api_key,
            base_url,
            model,
        } => {
            assert_eq!(name, "ollama");
            assert_eq!(provider, Provider::Ollama);
            assert_eq!(model, "qwen2.5-coder:7b-instruct-q4_K_M");
            assert_eq!(base_url.as_deref(), Some("http://localhost:11434/v1"));
            assert_eq!(api_key, "ollama");
        }
        ResolvedBackend::ClaudeCli { .. } => panic!("expected Api variant"),
    }
}

#[test]
fn entry_api_key_beats_environment() {
    let mut backends = HashMap::new();
    backends.insert(
        "deepseek".to_string(),
        BackendConfig::Api {
            provider: ApiProvider::DeepSeek,
            model: "deepseek-v4-pro".to_string(),
            base_url: None,
            api_key: Some("entry-key-123".to_string()),
            models: None,
        },
    );
    let settings = settings_with_backends(Some("deepseek"), backends);

    let resolved = resolve_active_backend(&settings, Path::new(".")).expect("should resolve");

    match resolved {
        ResolvedBackend::Api { api_key, .. } => assert_eq!(api_key, "entry-key-123"),
        ResolvedBackend::ClaudeCli { .. } => panic!("expected Api variant"),
    }
}

#[test]
fn unknown_default_backend_is_an_error() {
    let mut backends = HashMap::new();
    backends.insert(
        "deepseek".to_string(),
        BackendConfig::Api {
            provider: ApiProvider::DeepSeek,
            model: "deepseek-v4-pro".to_string(),
            base_url: None,
            api_key: Some("entry-key-123".to_string()),
            models: None,
        },
    );
    let settings = settings_with_backends(Some("nope"), backends);

    let err = resolve_active_backend(&settings, Path::new(".")).expect_err("should error");

    assert!(err.contains("nope"));
    assert!(err.contains("deepseek"));
}

#[test]
fn resolves_valid_claude_cli_entry() {
    let mut backends = HashMap::new();
    let mut env = HashMap::new();
    env.insert("CLAUDE_CLI_PATH".to_string(), "C:/tools/claude.exe".to_string());
    backends.insert(
        "claude".to_string(),
        BackendConfig::ClaudeCli {
            model: "opus".to_string(),
            permission_mode: Some("acceptEdits".to_string()),
            env: Some(env.clone()),
            models: None,
        },
    );
    let settings = settings_with_backends(Some("claude"), backends);

    let resolved = resolve_active_backend(&settings, Path::new(".")).expect("should resolve");

    match resolved {
        ResolvedBackend::ClaudeCli {
            name,
            model,
            permission_mode,
            env: resolved_env,
        } => {
            assert_eq!(name, "claude");
            assert_eq!(model, "opus");
            assert_eq!(permission_mode.as_deref(), Some("acceptEdits"));
            assert_eq!(resolved_env, Some(env));
        }
        ResolvedBackend::Api { .. } => panic!("expected ClaudeCli variant"),
    }
}

#[test]
fn claude_cli_entry_permission_mode_passes_through_none_when_omitted() {
    // `resolve_active_backend` carries the config's permission mode
    // through untouched. `ClaudeCliDriver::build_args` applies the
    // `bypassPermissions` default at spawn time. That default is covered
    // by `args_builder_defaults_permission_mode_to_bypass_permissions`
    // in `src/backend/claude_cli/process.rs`.
    let mut backends = HashMap::new();
    backends.insert(
        "claude".to_string(),
        BackendConfig::ClaudeCli {
            model: "opus".to_string(),
            permission_mode: None,
            env: None,
            models: None,
        },
    );
    let settings = settings_with_backends(Some("claude"), backends);

    let resolved = resolve_active_backend(&settings, Path::new(".")).expect("should resolve");

    match resolved {
        ResolvedBackend::ClaudeCli { permission_mode, .. } => {
            assert_eq!(permission_mode, None);
        }
        ResolvedBackend::Api { .. } => panic!("expected ClaudeCli variant"),
    }
}

#[test]
fn absent_default_backend_selects_deepseek() {
    let mut backends = HashMap::new();
    backends.insert(
        "deepseek".to_string(),
        BackendConfig::Api {
            provider: ApiProvider::DeepSeek,
            model: "deepseek-v4-pro".to_string(),
            base_url: None,
            api_key: Some("entry-key-123".to_string()),
            models: None,
        },
    );
    let settings = settings_with_backends(None, backends);

    let resolved = resolve_active_backend(&settings, Path::new(".")).expect("should resolve");

    match resolved {
        ResolvedBackend::Api { name, provider, .. } => {
            assert_eq!(name, "deepseek");
            assert_eq!(provider, Provider::DeepSeek);
        }
        ResolvedBackend::ClaudeCli { .. } => panic!("expected Api variant"),
    }
}

#[test]
fn default_backend_name_returns_configured_name() {
    let mut backends = HashMap::new();
    backends.insert(
        "claude".to_string(),
        BackendConfig::ClaudeCli {
            model: "opus".to_string(),
            permission_mode: None,
            env: None,
            models: None,
        },
    );
    let settings = settings_with_backends(Some("claude"), backends);
    let factory = BackendFactory::new(settings, PathBuf::from("."));

    assert_eq!(factory.default_backend_name(), "claude");
}

#[test]
fn default_backend_name_falls_back_to_deepseek_when_absent() {
    let settings = settings_with_backends(None, HashMap::new());
    let factory = BackendFactory::new(settings, PathBuf::from("."));

    assert_eq!(factory.default_backend_name(), "deepseek");
}

#[test]
fn build_with_model_override_produces_backend_carrying_the_override() {
    let mut backends = HashMap::new();
    backends.insert(
        "claude".to_string(),
        BackendConfig::ClaudeCli {
            model: "opus".to_string(),
            permission_mode: None,
            env: None,
            models: None,
        },
    );
    let settings = settings_with_backends(Some("claude"), backends);
    let factory = Arc::new(BackendFactory::new(settings, PathBuf::from(".")));
    let (tx, _rx) = mpsc::unbounded_channel();

    let backend = factory
        .build("claude", Some("sonnet"), tx, 0)
        .expect("should build");

    assert_eq!(*backend.model_flag().lock().unwrap(), "sonnet");
}

#[test]
fn build_with_unknown_name_names_the_request_and_lists_known_entries() {
    let mut backends = HashMap::new();
    backends.insert(
        "deepseek".to_string(),
        BackendConfig::Api {
            provider: ApiProvider::DeepSeek,
            model: "deepseek-v4-pro".to_string(),
            base_url: None,
            api_key: Some("entry-key-123".to_string()),
            models: None,
        },
    );
    let settings = settings_with_backends(Some("deepseek"), backends);
    let factory = Arc::new(BackendFactory::new(settings, PathBuf::from(".")));
    let (tx, _rx) = mpsc::unbounded_channel();

    let err = match factory.build("nope", None, tx, 0) {
        Ok(_) => panic!("should error on unknown name"),
        Err(e) => e,
    };

    assert!(err.contains("nope"));
    assert!(err.contains("deepseek"));
}

#[test]
fn may_dispatch_true_below_the_limit_false_at_it() {
    // Default depth limit is 2. The main session (depth 0) and a
    // depth-1 subagent may both dispatch further. A depth-2 subagent,
    // sitting at the limit, may not. Neither may one past it.
    assert!(may_dispatch(0, 2));
    assert!(may_dispatch(1, 2));
    assert!(!may_dispatch(2, 2));
    assert!(!may_dispatch(3, 2));
}

fn api_backend_settings() -> Settings {
    let mut backends = HashMap::new();
    backends.insert(
        "deepseek".to_string(),
        BackendConfig::Api {
            provider: ApiProvider::DeepSeek,
            model: "deepseek-v4-pro".to_string(),
            base_url: None,
            api_key: Some("entry-key-123".to_string()),
            models: None,
        },
    );
    settings_with_backends(Some("deepseek"), backends)
}

#[test]
fn task_tool_registered_below_the_depth_limit() {
    // Default max depth is 2. Depth 0, the main session, is below it.
    let factory = Arc::new(BackendFactory::new(api_backend_settings(), PathBuf::from(".")));
    let (tx, _rx) = mpsc::unbounded_channel();

    let backend = factory.build("deepseek", None, tx, 0).expect("should build");

    match backend {
        Backend::Api(agent) => assert!(agent.tool_names().iter().any(|n| n == "Task")),
        Backend::ClaudeCli(_) => panic!("expected Api variant"),
    }
}

#[test]
fn task_tool_absent_at_the_depth_limit() {
    // Default max depth is 2. Depth 2 sits at the limit, so no Task tool
    // goes in and the dispatch chain stops there.
    let factory = Arc::new(BackendFactory::new(api_backend_settings(), PathBuf::from(".")));
    let (tx, _rx) = mpsc::unbounded_channel();

    let backend = factory.build("deepseek", None, tx, 2).expect("should build");

    match backend {
        Backend::Api(agent) => assert!(!agent.tool_names().iter().any(|n| n == "Task")),
        Backend::ClaudeCli(_) => panic!("expected Api variant"),
    }
}

#[test]
fn built_backend_carries_the_factorys_injected_interrupt_flag() {
    let injected = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let factory = Arc::new(
        BackendFactory::new(api_backend_settings(), PathBuf::from("."))
            .with_interrupt_flag(injected.clone()),
    );
    let (tx, _rx) = mpsc::unbounded_channel();

    let backend = factory.build("deepseek", None, tx, 0).expect("should build");

    // Set the flag through the handle the factory was given, not through
    // the backend's own getter, then check the backend reads it as true.
    injected.store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(backend.interrupt_flag().load(std::sync::atomic::Ordering::SeqCst));
}
