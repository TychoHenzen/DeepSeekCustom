//! Unit tests for `deepseek_custom::gui::backend_picker` (`src/gui/backend_picker.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use deepseek_custom::agent::events::AgentCommand;
use deepseek_custom::config::settings::{ApiProvider, BackendConfig, Settings};
use deepseek_custom::gui::backend_picker::{BackendPicker, apply_backend_model};

fn settings_with_two_backends() -> Settings {
    let mut backends = HashMap::new();
    backends.insert(
        "deepseek".to_string(),
        BackendConfig::Api {
            provider: ApiProvider::DeepSeek,
            model: "deepseek-v4-flash".to_string(),
            base_url: None,
            api_key: None,
            models: None,
        },
    );
    backends.insert(
        "claude".to_string(),
        BackendConfig::ClaudeCli {
            model: "opus".to_string(),
            permission_mode: None,
            env: None,
            models: None,
        },
    );
    Settings {
        backends: Some(backends),
        default_backend: Some("deepseek".to_string()),
        ..Settings::default()
    }
}

fn picker_on(settings: &Settings, model: &str) -> BackendPicker {
    BackendPicker::new(settings, Arc::new(Mutex::new(model.to_string())))
}

#[test]
fn new_sorts_the_backend_names() {
    let picker = picker_on(&settings_with_two_backends(), "deepseek-v4-flash");
    assert_eq!(picker.options(), ["claude", "deepseek"]);
}

#[test]
fn repo_settings_make_codex_selectable_in_the_backend_picker() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let contents = std::fs::read_to_string(repo_root.join("settings.json")).unwrap();
    let settings: Settings = serde_json::from_str(&contents).unwrap();
    let picker = picker_on(&settings, "deepseek-v4-flash");

    assert!(picker.options().contains(&"codex".to_string()));
}

#[test]
fn new_selects_the_default_backend() {
    let picker = picker_on(&settings_with_two_backends(), "deepseek-v4-flash");
    assert_eq!(picker.selected_name(), Some("deepseek"));
    assert_eq!(picker.active_backend(), "deepseek");
}

#[test]
fn new_falls_back_to_the_first_backend_when_no_default_is_named() {
    let mut settings = settings_with_two_backends();
    settings.default_backend = None;
    let picker = picker_on(&settings, "deepseek-v4-flash");
    assert_eq!(picker.selected_name(), Some("claude"));
}

#[test]
fn new_falls_back_to_the_first_backend_on_an_unknown_default() {
    let mut settings = settings_with_two_backends();
    settings.default_backend = Some("not_a_backend".to_string());
    let picker = picker_on(&settings, "deepseek-v4-flash");
    assert_eq!(picker.selected_name(), Some("claude"));
}

#[test]
fn new_seeds_the_model_dropdown_with_the_running_model() {
    let picker = picker_on(&settings_with_two_backends(), "deepseek-v4-flash");
    assert_eq!(picker.model(), "deepseek-v4-flash");
    assert_eq!(picker.model_options(), ["deepseek-v4-flash"]);
}

#[test]
fn switching_backend_adopts_that_entrys_declared_model() {
    let mut settings = settings_with_two_backends();
    let mut picker = picker_on(&settings, "deepseek-v4-flash");
    assert!(
        picker
            .switch_backend("claude".to_string(), &mut settings)
            .is_some()
    );
    assert_eq!(picker.model(), "opus");
    assert_eq!(
        picker.model_options(),
        ["opus"],
        "the dropdown reseeds with the new backend's declared model"
    );
    assert_eq!(settings.default_backend.as_deref(), Some("claude"));
}

#[test]
fn switching_backend_moves_the_shared_model_handle_to_the_new_entry() {
    let mut settings = settings_with_two_backends();
    let flag = Arc::new(Mutex::new("deepseek-v4-flash".to_string()));
    let mut picker = BackendPicker::new(&settings, Arc::clone(&flag));

    picker.switch_backend("claude".to_string(), &mut settings);

    assert_eq!(
        flag.lock().unwrap().as_str(),
        "opus",
        "the switch is live, so the incoming entry's model is the running one"
    );
    assert_eq!(
        picker.active_backend(),
        "claude",
        "the running backend moves in the same call, not on the next start"
    );
}

#[test]
fn switching_backend_reports_the_outgoing_backend_for_the_session_save() {
    let mut settings = settings_with_two_backends();
    let mut picker = picker_on(&settings, "deepseek-v4-flash");

    let switch = picker
        .switch_backend("claude".to_string(), &mut settings)
        .expect("a known backend produces a switch");

    assert_eq!(switch.outgoing.backend, "deepseek");
    assert_eq!(switch.outgoing.model, "deepseek-v4-flash");
}

#[test]
fn switching_backend_asks_the_agent_for_the_named_entry_and_its_model() {
    let mut settings = settings_with_two_backends();
    let mut picker = picker_on(&settings, "deepseek-v4-flash");

    let switch = picker
        .switch_backend("claude".to_string(), &mut settings)
        .expect("a known backend produces a switch");

    match switch.command {
        AgentCommand::SwitchBackend { name, model } => {
            assert_eq!(name, "claude");
            assert_eq!(model.as_deref(), Some("opus"));
        }
        other => panic!("expected a SwitchBackend command, got {other:?}"),
    }
}

#[test]
fn switching_to_an_unknown_backend_asks_the_agent_for_nothing() {
    let mut settings = settings_with_two_backends();
    let mut picker = picker_on(&settings, "deepseek-v4-flash");

    let switch = picker.switch_backend("not_a_backend".to_string(), &mut settings);

    assert!(
        switch.is_none(),
        "a name the factory could not build must not reach the agent"
    );
    assert_eq!(
        picker.active_backend(),
        "deepseek",
        "the running backend stays where it was"
    );
}

#[test]
fn switching_model_on_the_running_backend_writes_the_shared_handle() {
    let mut settings = settings_with_two_backends();
    let flag = Arc::new(Mutex::new("deepseek-v4-flash".to_string()));
    let mut picker = BackendPicker::new(&settings, Arc::clone(&flag));

    assert!(picker.switch_model("deepseek-v4-pro".to_string(), &mut settings));

    assert_eq!(flag.lock().unwrap().as_str(), "deepseek-v4-pro");
    assert_eq!(picker.model(), "deepseek-v4-pro");
}

/// After a backend switch, the model dropdown drives the backend that is
/// now running. Picking a model that belongs to the newly selected entry
/// reaches the agent, because that entry is the running one.
#[test]
fn switching_model_after_a_backend_switch_writes_the_shared_handle() {
    let mut settings = settings_with_two_backends();
    let flag = Arc::new(Mutex::new("deepseek-v4-flash".to_string()));
    let mut picker = BackendPicker::new(&settings, Arc::clone(&flag));
    picker.switch_backend("claude".to_string(), &mut settings);
    picker.set_selected_idx(0); // now pointing at "claude"

    picker.switch_model("sonnet".to_string(), &mut settings);

    assert_eq!(
        flag.lock().unwrap().as_str(),
        "sonnet",
        "the switch already made claude the running backend"
    );
}

#[test]
fn switching_model_persists_onto_the_selected_backends_entry() {
    let mut settings = settings_with_two_backends();
    let mut picker = picker_on(&settings, "deepseek-v4-flash");
    picker.switch_model("deepseek-v4-pro".to_string(), &mut settings);
    let cfg = settings.resolve_backend("deepseek").expect("must resolve");
    assert_eq!(cfg.model(), "deepseek-v4-pro");
}

#[test]
fn a_fetched_list_for_the_selected_backend_replaces_the_options() {
    let settings = settings_with_two_backends();
    let mut picker = picker_on(&settings, "deepseek-v4-flash");
    picker.apply_fetched_list(
        "deepseek",
        vec!["deepseek-v4-flash".into(), "deepseek-v4-pro".into()],
    );
    assert_eq!(
        picker.model_options(),
        ["deepseek-v4-flash", "deepseek-v4-pro"]
    );
}

#[test]
fn a_fetched_list_always_holds_the_current_model() {
    let settings = settings_with_two_backends();
    let mut picker = picker_on(&settings, "deepseek-v4-flash");
    picker.apply_fetched_list("deepseek", vec!["something-else".into()]);
    assert!(
        picker
            .model_options()
            .contains(&"deepseek-v4-flash".to_string()),
        "the dropdown must always offer its own selection"
    );
}

#[test]
fn a_stale_fetched_list_is_dropped() {
    let settings = settings_with_two_backends();
    let mut picker = picker_on(&settings, "deepseek-v4-flash");
    picker.apply_fetched_list("claude", vec!["opus".into(), "sonnet".into()]);
    assert_eq!(
        picker.model_options(),
        ["deepseek-v4-flash"],
        "a result for another backend must not land here"
    );
}

#[test]
fn draining_an_empty_channel_yields_nothing() {
    let settings = settings_with_two_backends();
    let mut picker = picker_on(&settings, "deepseek-v4-flash");
    assert!(picker.drain_fetched_lists().is_empty());
}

#[test]
fn draining_takes_every_queued_result() {
    let settings = settings_with_two_backends();
    let mut picker = picker_on(&settings, "deepseek-v4-flash");
    picker.send_fetched_list_for_test("deepseek".to_string(), vec!["a".into()]);
    picker.send_fetched_list_for_test("claude".to_string(), vec!["b".into()]);
    assert_eq!(picker.drain_fetched_lists().len(), 2);
    assert!(picker.drain_fetched_lists().is_empty());
}

#[test]
fn apply_backend_model_ignores_an_unknown_backend() {
    let mut settings = settings_with_two_backends();
    apply_backend_model(&mut settings, "not_a_backend", "whatever");
    let cfg = settings.resolve_backend("deepseek").expect("must resolve");
    assert_eq!(cfg.model(), "deepseek-v4-flash");
}

#[test]
fn apply_backend_model_writes_a_claude_entry_too() {
    let mut settings = settings_with_two_backends();
    apply_backend_model(&mut settings, "claude", "haiku");
    let cfg = settings.resolve_backend("claude").expect("must resolve");
    assert_eq!(cfg.model(), "haiku");
}
