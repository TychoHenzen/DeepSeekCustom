use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use deepseek_custom::agent::events::AgentCommand;
use deepseek_custom::agent::repeat::RepeatCommand;
use deepseek_custom::application::services::{
    ApplicationServicePorts, RuntimeSettingsPort, ServiceCommand, ServiceDispatchError,
    ServiceKind, SettingsController,
};
use deepseek_custom::config::settings::{ApiProvider, BackendConfig, Settings};
use deepseek_custom::effort::Effort;
use deepseek_custom::voice::service::VoiceCommand;
use std::collections::HashMap;
use tokio::sync::mpsc;

#[test]
fn typed_ports_preserve_domain_command_mapping() {
    let (agent_tx, mut agent_rx) = mpsc::unbounded_channel();
    let (repeat_tx, mut repeat_rx) = mpsc::unbounded_channel();
    let (voice_tx, mut voice_rx) = mpsc::unbounded_channel();
    let ports = ApplicationServicePorts::default()
        .with_agent(agent_tx)
        .with_autopilot(repeat_tx)
        .with_voice(voice_tx);

    ports
        .dispatch(ServiceCommand::Agent(AgentCommand::NewSession))
        .unwrap();
    ports
        .dispatch(ServiceCommand::Autopilot(RepeatCommand {
            task: "inspect".into(),
            iterations: 3,
        }))
        .unwrap();
    ports
        .dispatch(ServiceCommand::Voice(VoiceCommand::StartListening))
        .unwrap();

    assert!(matches!(agent_rx.try_recv(), Ok(AgentCommand::NewSession)));
    let repeat = repeat_rx.try_recv().unwrap();
    assert_eq!(repeat.task, "inspect");
    assert_eq!(repeat.iterations, 3);
    assert_eq!(voice_rx.try_recv().unwrap(), VoiceCommand::StartListening);
    assert_eq!(
        ApplicationServicePorts::default()
            .dispatch(ServiceCommand::Agent(AgentCommand::NewSession)),
        Err(ServiceDispatchError::Unavailable(ServiceKind::Agent))
    );
}

// covers: deepseek-custom/web-application :: Settings preserve runtime and persistence boundaries :: User changes an existing setting
#[test]
fn visible_setting_updates_runtime_and_existing_schema_without_exposing_secrets() {
    let root = super::scratch_dir("application-settings", "visible-update");
    let effort = Arc::new(AtomicU8::new(0));
    let voice_mode = Arc::new(AtomicBool::new(false));
    let context = Arc::new(AtomicUsize::new(32_000));
    let model = Arc::new(Mutex::new("old-model".to_string()));
    let working = Arc::new(Mutex::new(root.clone()));
    let plain = Arc::new(AtomicBool::new(false));
    let grade = Arc::new(AtomicU8::new(8));
    let runtime = RuntimeSettingsPort::new(
        root.clone(),
        effort.clone(),
        voice_mode,
        context.clone(),
        model.clone(),
        working,
        plain,
        grade,
    );
    let mut stored = Settings {
        api_key: Some("top-secret".into()),
        ..Settings::default()
    };
    stored.backends = Some(HashMap::from([(
        "api".into(),
        BackendConfig::Api {
            provider: ApiProvider::DeepSeek,
            model: "old-model".into(),
            base_url: None,
            api_key: Some("backend-secret".into()),
            models: Some(vec!["old-model".into(), "new-model".into()]),
        },
    )]));
    let controller = SettingsController::new(
        root.clone(),
        stored,
        runtime,
        Some("api".into()),
        Some("old-model".into()),
    );
    let mut visible = controller.visible();
    visible.selected_model = Some("new-model".into());
    visible.effort = "high".into();
    visible.context_budget = 120_000;
    visible.max_tokens = 32_768;
    visible.show_raw_output = true;
    visible.style.plain_language = true;
    visible.voice.tts_enabled = true;
    visible.procedure.localization_backend = Some("api".into());

    let updated = controller.update(visible).unwrap();
    let browser_json = serde_json::to_string(&updated).unwrap();
    let persisted = std::fs::read_to_string(root.join("settings.json")).unwrap();
    assert_eq!(Effort::load(&effort), Effort::High);
    assert_eq!(context.load(Ordering::SeqCst), 120_000);
    assert_eq!(*model.lock().unwrap(), "new-model");
    assert!(persisted.contains("\"max_tokens\": 32768"));
    assert!(persisted.contains("top-secret"));
    assert!(!browser_json.contains("top-secret"));
    assert!(!browser_json.contains("backend-secret"));
    assert!(!browser_json.contains("api_key"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn runtime_effects_keep_project_root_fixed_while_working_dir_changes() {
    let project_root = PathBuf::from("fixed-project");
    let effort = Arc::new(AtomicU8::new(0));
    let voice_mode = Arc::new(AtomicBool::new(false));
    let context_budget = Arc::new(AtomicUsize::new(1));
    let model = Arc::new(Mutex::new("old".to_string()));
    let working_dir = Arc::new(Mutex::new(project_root.clone()));
    let plain = Arc::new(AtomicBool::new(false));
    let grade = Arc::new(AtomicU8::new(8));
    let port = RuntimeSettingsPort::new(
        project_root.clone(),
        effort.clone(),
        voice_mode.clone(),
        context_budget.clone(),
        model.clone(),
        working_dir,
        plain.clone(),
        grade.clone(),
    );

    port.set_working_dir(PathBuf::from("other-working-dir"));
    port.set_model("new".into());
    port.apply_visible_effects(Effort::High, 150_000, true, true, 12);

    assert_eq!(port.project_root(), project_root.as_path());
    assert_eq!(port.working_dir(), PathBuf::from("other-working-dir"));
    assert_eq!(*model.lock().unwrap(), "new");
    assert_eq!(Effort::load(&effort), Effort::High);
    assert_eq!(context_budget.load(Ordering::SeqCst), 150_000);
    assert!(voice_mode.load(Ordering::SeqCst));
    assert!(plain.load(Ordering::SeqCst));
    assert_eq!(grade.load(Ordering::SeqCst), 12);
}
