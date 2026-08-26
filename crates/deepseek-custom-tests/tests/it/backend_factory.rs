//! Unit tests for `deepseek_custom::backend::factory` (`src/backend/factory.rs`),
//! including the former `src/backend/factory_tests.rs` submodule, merged
//! here as part of the two-crate workspace split.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use deepseek_custom::agent::events::SubagentId;
use deepseek_custom::api::provider::Provider;
use deepseek_custom::backend::factory::{BackendFactory, may_dispatch};
use deepseek_custom::backend::resolved::{ResolvedBackend, resolve_active_backend};
use deepseek_custom::backend::stub::StubBackend;
use deepseek_custom::backend::{Backend, SharedFlags};
use deepseek_custom::config::settings::{ApiProvider, BackendConfig, Settings};
use deepseek_custom::effort::Effort;

use tokio::sync::mpsc;

fn settings_with_backends(
    default_backend: Option<&str>,
    backends: HashMap<String, BackendConfig>,
) -> Settings {
    Settings {
        default_backend: default_backend.map(|s| s.to_string()),
        backends: Some(backends),
        ..Settings::default()
    }
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
        ResolvedBackend::CodexCli { .. } => panic!("expected Api variant"),
        ResolvedBackend::Stub { .. } => panic!("expected Api variant"),
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
        ResolvedBackend::CodexCli { .. } => panic!("expected Api variant"),
        ResolvedBackend::Stub { .. } => panic!("expected Api variant"),
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
    env.insert(
        "CLAUDE_CLI_PATH".to_string(),
        "C:/tools/claude.exe".to_string(),
    );
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
        ResolvedBackend::CodexCli { .. } => panic!("expected ClaudeCli variant"),
        ResolvedBackend::Stub { .. } => panic!("expected ClaudeCli variant"),
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
        ResolvedBackend::ClaudeCli {
            permission_mode, ..
        } => {
            assert_eq!(permission_mode, None);
        }
        ResolvedBackend::Api { .. } => panic!("expected ClaudeCli variant"),
        ResolvedBackend::CodexCli { .. } => panic!("expected ClaudeCli variant"),
        ResolvedBackend::Stub { .. } => panic!("expected ClaudeCli variant"),
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
        ResolvedBackend::CodexCli { .. } => panic!("expected Api variant"),
        ResolvedBackend::Stub { .. } => panic!("expected Api variant"),
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

fn codex_cli_backend_settings() -> Settings {
    let mut backends = HashMap::new();
    backends.insert(
        "codex".to_string(),
        BackendConfig::CodexCli {
            model: "o3".to_string(),
            sandbox: Some("workspace-write".to_string()),
            env: None,
            models: None,
        },
    );
    settings_with_backends(Some("codex"), backends)
}

#[test]
fn codex_cli_named_entry_builds_variant_with_model_override() {
    let factory = Arc::new(BackendFactory::new(
        codex_cli_backend_settings(),
        PathBuf::from("."),
    ));
    let (tx, _rx) = mpsc::unbounded_channel();

    let backend = factory
        .build("codex", Some("o4-mini"), tx, 0)
        .expect("should build CodexCli backend");

    match backend {
        Backend::CodexCli(driver) => {
            assert_eq!(*driver.model_flag().lock().unwrap(), "o4-mini");
        }
        Backend::Api(_) => panic!("expected CodexCli variant"),
        Backend::ClaudeCli(_) => panic!("expected CodexCli variant"),
        Backend::Stub(_) => panic!("expected CodexCli variant"),
    }
}

#[test]
fn codex_cli_depth_zero_adopts_shared_flags_and_depth_one_does_not() {
    let flags = flags_for_test();
    let factory = Arc::new(
        BackendFactory::new(codex_cli_backend_settings(), PathBuf::from("."))
            .with_session_flags(flags.clone()),
    );
    let (tx, _rx) = mpsc::unbounded_channel();

    let main_backend = factory
        .build("codex", None, tx.clone(), 0)
        .expect("should build main CodexCli backend");
    let subagent_backend = factory
        .build("codex", None, tx, 1)
        .expect("should build subagent CodexCli backend");

    assert!(matches!(main_backend, Backend::CodexCli(_)));
    assert!(Arc::ptr_eq(&main_backend.model_flag(), &flags.model));
    assert!(Arc::ptr_eq(
        &main_backend.interrupt_flag(),
        &flags.interrupt
    ));
    assert!(matches!(subagent_backend, Backend::CodexCli(_)));
    assert!(!Arc::ptr_eq(&subagent_backend.model_flag(), &flags.model));
    assert!(!Arc::ptr_eq(
        &subagent_backend.interrupt_flag(),
        &flags.interrupt
    ));
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
    let factory = Arc::new(BackendFactory::new(
        api_backend_settings(),
        PathBuf::from("."),
    ));
    let (tx, _rx) = mpsc::unbounded_channel();

    let backend = factory
        .build("deepseek", None, tx, 0)
        .expect("should build");

    match backend {
        Backend::Api(agent) => assert!(agent.tool_names().iter().any(|n| n == "Task")),
        Backend::ClaudeCli(_) => panic!("expected Api variant"),
        Backend::CodexCli(_) => panic!("expected Api variant"),
        Backend::Stub(_) => panic!("expected Api variant"),
    }
}

#[test]
fn task_tool_absent_at_the_depth_limit() {
    // Default max depth is 2. Depth 2 sits at the limit, so no Task tool
    // goes in and the dispatch chain stops there.
    let factory = Arc::new(BackendFactory::new(
        api_backend_settings(),
        PathBuf::from("."),
    ));
    let (tx, _rx) = mpsc::unbounded_channel();

    let backend = factory
        .build("deepseek", None, tx, 2)
        .expect("should build");

    match backend {
        Backend::Api(agent) => assert!(!agent.tool_names().iter().any(|n| n == "Task")),
        Backend::ClaudeCli(_) => panic!("expected Api variant"),
        Backend::CodexCli(_) => panic!("expected Api variant"),
        Backend::Stub(_) => panic!("expected Api variant"),
    }
}

/// `SendMessage` is gated by the exact same depth check as `Task`: below
/// the limit, a backend gets both, since a session it cannot open through
/// `Task` is never reachable through `SendMessage` either.
#[test]
fn send_message_tool_registered_below_the_depth_limit() {
    let factory = Arc::new(BackendFactory::new(
        api_backend_settings(),
        PathBuf::from("."),
    ));
    let (tx, _rx) = mpsc::unbounded_channel();

    let backend = factory
        .build("deepseek", None, tx, 0)
        .expect("should build");

    match backend {
        Backend::Api(agent) => assert!(agent.tool_names().iter().any(|n| n == "SendMessage")),
        Backend::ClaudeCli(_) => panic!("expected Api variant"),
        Backend::CodexCli(_) => panic!("expected Api variant"),
        Backend::Stub(_) => panic!("expected Api variant"),
    }
}

/// At the depth limit, a subagent's registry carries neither `Task` nor
/// `SendMessage`.
#[test]
fn send_message_tool_absent_at_the_depth_limit() {
    let factory = Arc::new(BackendFactory::new(
        api_backend_settings(),
        PathBuf::from("."),
    ));
    let (tx, _rx) = mpsc::unbounded_channel();

    let backend = factory
        .build("deepseek", None, tx, 2)
        .expect("should build");

    match backend {
        Backend::Api(agent) => assert!(!agent.tool_names().iter().any(|n| n == "SendMessage")),
        Backend::ClaudeCli(_) => panic!("expected Api variant"),
        Backend::CodexCli(_) => panic!("expected Api variant"),
        Backend::Stub(_) => panic!("expected Api variant"),
    }
}

/// `CloseSession` is gated by the exact same depth check as `Task` and
/// `SendMessage`: all three appear together below the limit.
#[test]
fn close_session_tool_registered_below_the_depth_limit() {
    let factory = Arc::new(BackendFactory::new(
        api_backend_settings(),
        PathBuf::from("."),
    ));
    let (tx, _rx) = mpsc::unbounded_channel();

    let backend = factory
        .build("deepseek", None, tx, 0)
        .expect("should build");

    match backend {
        Backend::Api(agent) => assert!(agent.tool_names().iter().any(|n| n == "CloseSession")),
        Backend::ClaudeCli(_) => panic!("expected Api variant"),
        Backend::CodexCli(_) => panic!("expected Api variant"),
        Backend::Stub(_) => panic!("expected Api variant"),
    }
}

/// At the depth limit, a subagent's registry carries none of `Task`,
/// `SendMessage`, or `CloseSession`: all three disappear together.
#[test]
fn close_session_tool_absent_at_the_depth_limit() {
    let factory = Arc::new(BackendFactory::new(
        api_backend_settings(),
        PathBuf::from("."),
    ));
    let (tx, _rx) = mpsc::unbounded_channel();

    let backend = factory
        .build("deepseek", None, tx, 2)
        .expect("should build");

    match backend {
        Backend::Api(agent) => {
            assert!(!agent.tool_names().iter().any(|n| n == "Task"));
            assert!(!agent.tool_names().iter().any(|n| n == "SendMessage"));
            assert!(!agent.tool_names().iter().any(|n| n == "CloseSession"));
        }
        Backend::ClaudeCli(_) => panic!("expected Api variant"),
        Backend::CodexCli(_) => panic!("expected Api variant"),
        Backend::Stub(_) => panic!("expected Api variant"),
    }
}

/// The Phase 3 lifetime rule from the roadmap: a subagent session lives
/// until *its parent's* turn ends, or until it is closed, whichever comes
/// first. Not until *any* agent's turn ends. Two agents built off the same
/// factory, exactly as a main session and a subagent are, must each get
/// their own registry. Against a factory that hands out one shared
/// registry to every backend it builds, agent A's `Reset` would empty
/// agent B's registry too, since both would hold the literal same `Arc`.
/// This test fails on that build and passes once each agent owns its own.
#[tokio::test]
async fn one_agents_reset_does_not_close_another_agents_session() {
    let factory = Arc::new(BackendFactory::new(
        api_backend_settings(),
        PathBuf::from("."),
    ));
    let (tx_a, _rx_a) = mpsc::unbounded_channel();
    let (tx_b, _rx_b) = mpsc::unbounded_channel();

    let backend_a = factory
        .build("deepseek", None, tx_a, 0)
        .expect("should build");
    let backend_b = factory
        .build("deepseek", None, tx_b, 0)
        .expect("should build");

    let Backend::Api(agent_a) = backend_a else {
        panic!("expected Api variant");
    };
    let Backend::Api(agent_b) = backend_b else {
        panic!("expected Api variant");
    };

    let registry_a = agent_a
        .subagent_registry_for_test()
        .expect("build_api_backend should set a registry");
    let registry_b = agent_b
        .subagent_registry_for_test()
        .expect("build_api_backend should set a registry");

    let stub_session = || {
        Backend::Stub(Box::new(StubBackend::new(
            Vec::new(),
            "stub-model".to_string(),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicUsize::new(0)),
        )))
    };
    let id_a = SubagentId::next();
    let id_b = SubagentId::next();
    registry_a.register(id_a, stub_session()).await;
    registry_b.register(id_b, stub_session()).await;

    let output = agent_a
        .execute_tool_for_test("reset", "{\"prompt\":\"start fresh\"}")
        .await;

    assert!(!output.is_error);
    assert!(
        !registry_a.contains(id_a).await,
        "agent A's own session should close on its own reset"
    );
    assert!(
        registry_b.contains(id_b).await,
        "agent B's session must not be closed by agent A's reset"
    );
    assert_eq!(registry_b.len().await, 1);
}

#[test]
fn built_backend_carries_the_factorys_injected_interrupt_flag() {
    let injected = Arc::new(AtomicBool::new(false));
    let factory = Arc::new(
        BackendFactory::new(api_backend_settings(), PathBuf::from("."))
            .with_interrupt_flag(injected.clone()),
    );
    let (tx, _rx) = mpsc::unbounded_channel();

    let backend = factory
        .build("deepseek", None, tx, 0)
        .expect("should build");

    // Set the flag through the handle the factory was given, not through
    // the backend's own getter, then check the backend reads it as true.
    injected.store(true, Ordering::SeqCst);
    assert!(backend.interrupt_flag().load(Ordering::SeqCst));
}

/// `with_working_dir` reports the exact `Arc` it was given, and that value
/// is independent of the original factory's own: writing through either
/// handle after the split never moves the other. This is the isolation
/// P4S05's subagent `working_dir` override relies on.
#[test]
fn with_working_dir_reports_the_given_arc_independent_of_the_original() {
    let factory = Arc::new(BackendFactory::new(Settings::default(), PathBuf::from(".")));
    let original_snapshot = factory.working_dir_snapshot_for_test();

    let replacement = Arc::new(std::sync::Mutex::new(PathBuf::from("C:/subagent-only")));
    let sub_factory = factory.with_working_dir_for_test(Arc::clone(&replacement));

    assert_eq!(
        *sub_factory.working_dir().lock().unwrap(),
        PathBuf::from("C:/subagent-only")
    );

    *sub_factory.working_dir().lock().unwrap() = PathBuf::from("C:/moved-by-subagent");
    assert_eq!(
        factory.working_dir_snapshot_for_test(),
        original_snapshot,
        "writing through the split-off factory must never move the original"
    );

    *factory.working_dir().lock().unwrap() = PathBuf::from("C:/moved-by-parent");
    assert_eq!(
        *sub_factory.working_dir().lock().unwrap(),
        PathBuf::from("C:/moved-by-subagent"),
        "writing through the original must never move the split-off factory"
    );
}

// â”€â”€ Session flag adoption â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
//
// The GUI holds one set of handles for the life of the process, and every
// backend built for its session adopts them. That is what makes a backend
// switch at runtime possible: Escape, the effort control, the model picker,
// and the voice-mode toggle all keep driving whichever backend is current.
// A subagent must not adopt them, or its own effort level and model would
// move the session's.

fn flags_for_test() -> SharedFlags {
    SharedFlags::new("seeded-model".to_string())
}

#[test]
fn a_main_session_backend_adopts_the_guis_handles() {
    let flags = flags_for_test();
    let factory = Arc::new(
        BackendFactory::new(api_backend_settings(), PathBuf::from("."))
            .with_session_flags(flags.clone()),
    );
    let (tx, _rx) = mpsc::unbounded_channel();

    let backend = factory
        .build("deepseek", None, tx, 0)
        .expect("should build");

    assert!(Arc::ptr_eq(&backend.effort_flag(), &flags.effort));
    assert!(Arc::ptr_eq(&backend.model_flag(), &flags.model));
    assert!(Arc::ptr_eq(&backend.voice_mode_flag(), &flags.voice_mode));
    assert!(Arc::ptr_eq(
        &backend.context_budget_flag(),
        &flags.context_budget
    ));
    assert!(Arc::ptr_eq(&backend.interrupt_flag(), &flags.interrupt));
    assert!(Arc::ptr_eq(
        &backend.repeat_interrupt_flag(),
        &flags.repeat_interrupt
    ));
}

#[test]
fn a_subagent_backend_keeps_its_own_handles() {
    let flags = flags_for_test();
    let factory = Arc::new(
        BackendFactory::new(api_backend_settings(), PathBuf::from("."))
            .with_session_flags(flags.clone()),
    );
    let (tx, _rx) = mpsc::unbounded_channel();

    let backend = factory
        .build("deepseek", None, tx, 1)
        .expect("should build");

    assert!(
        !Arc::ptr_eq(&backend.effort_flag(), &flags.effort),
        "a subagent's effort level must never move the session's"
    );
    assert!(
        !Arc::ptr_eq(&backend.model_flag(), &flags.model),
        "a subagent's model must never move the session's"
    );
}

/// Two backends built one after the other, as a runtime switch does, answer
/// to the same handles. This is the invariant the switch rests on: the GUI
/// keeps writing the handles it was given at startup, and the replacement
/// reads them.
#[test]
fn a_replacement_backend_answers_to_the_same_handles_as_the_one_before_it() {
    let flags = flags_for_test();
    let factory = Arc::new(
        BackendFactory::new(api_backend_settings(), PathBuf::from("."))
            .with_session_flags(flags.clone()),
    );
    let (tx, _rx) = mpsc::unbounded_channel();

    let first = factory
        .build("deepseek", None, tx.clone(), 0)
        .expect("should build");
    let second = factory
        .build("deepseek", None, tx, 0)
        .expect("should build");

    assert!(Arc::ptr_eq(&first.effort_flag(), &second.effort_flag()));
    assert!(Arc::ptr_eq(&first.model_flag(), &second.model_flag()));
    assert!(Arc::ptr_eq(
        &first.interrupt_flag(),
        &second.interrupt_flag()
    ));
}

/// The `Api` path hands its own effort handle to the `Task` tool it builds,
/// so adopting a different one afterwards would leave that tool reading a
/// flag nothing writes. The factory therefore seeds the tool from the
/// session handle at construction instead. Proven through the tool: a write
/// to the GUI's handle is what a dispatch reads as the inherited level.
#[test]
fn the_task_tool_reads_the_same_effort_handle_the_gui_writes() {
    let flags = flags_for_test();
    let factory = Arc::new(
        BackendFactory::new(api_backend_settings(), PathBuf::from("."))
            .with_session_flags(flags.clone()),
    );
    let (tx, _rx) = mpsc::unbounded_channel();

    let backend = factory
        .build("deepseek", None, tx, 0)
        .expect("should build");

    Effort::Max.store(&flags.effort);
    assert_eq!(
        Effort::load(&backend.effort_flag()),
        Effort::Max,
        "the level the sidebar set is the level a dispatch inherits"
    );
}
