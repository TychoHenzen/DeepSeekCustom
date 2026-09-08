//! Typed, presentation-neutral ports to long-lived domain services.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures::future::join_all;
use tokio::sync::mpsc;

use crate::agent::events::AgentCommand;
use crate::agent::repeat::RepeatCommand;
use crate::api::models::list_models;
use crate::application::dto::{AppError, AppErrorCode, VisibleSettings};
use crate::config::settings::{BackendConfig, Settings, TriggerMode};
use crate::effort::Effort;
use crate::procedure::ProcedureCommand;
use crate::search::SearchCommand;
use crate::voice::service::VoiceCommand;

/// A narrow sender used by temporary native adapters. It hides the channel
/// type while preserving the domain command and its send result.
#[derive(Clone)]
pub struct DomainCommandPort<T>(mpsc::UnboundedSender<T>);

impl<T> DomainCommandPort<T> {
    pub fn new(sender: mpsc::UnboundedSender<T>) -> Self {
        Self(sender)
    }

    pub fn send(&self, command: T) -> Result<(), mpsc::error::SendError<T>> {
        self.0.send(command)
    }
}

pub enum ServiceCommand {
    Agent(AgentCommand),
    Autopilot(RepeatCommand),
    Search(SearchCommand),
    Procedure(ProcedureCommand),
    Voice(VoiceCommand),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceKind {
    Agent,
    Autopilot,
    Search,
    Procedure,
    Voice,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceDispatchError {
    Unavailable(ServiceKind),
    Closed(ServiceKind),
}

/// Process-owned service senders. Presentation code cannot receive or mutate
/// service state through this value.
#[derive(Default)]
pub struct ApplicationServicePorts {
    agent: Option<mpsc::UnboundedSender<AgentCommand>>,
    autopilot: Option<mpsc::UnboundedSender<RepeatCommand>>,
    search: Option<mpsc::UnboundedSender<SearchCommand>>,
    procedure: Option<mpsc::UnboundedSender<ProcedureCommand>>,
    voice: Option<mpsc::UnboundedSender<VoiceCommand>>,
}

impl ApplicationServicePorts {
    pub fn with_agent(mut self, sender: mpsc::UnboundedSender<AgentCommand>) -> Self {
        self.agent = Some(sender);
        self
    }

    pub fn with_autopilot(mut self, sender: mpsc::UnboundedSender<RepeatCommand>) -> Self {
        self.autopilot = Some(sender);
        self
    }

    pub fn with_voice(mut self, sender: mpsc::UnboundedSender<VoiceCommand>) -> Self {
        self.voice = Some(sender);
        self
    }

    pub fn dispatch(&self, command: ServiceCommand) -> Result<(), ServiceDispatchError> {
        match command {
            ServiceCommand::Agent(command) => send(&self.agent, command, ServiceKind::Agent),
            ServiceCommand::Autopilot(command) => {
                send(&self.autopilot, command, ServiceKind::Autopilot)
            }
            ServiceCommand::Search(command) => send(&self.search, command, ServiceKind::Search),
            ServiceCommand::Procedure(command) => {
                send(&self.procedure, command, ServiceKind::Procedure)
            }
            ServiceCommand::Voice(command) => send(&self.voice, command, ServiceKind::Voice),
        }
    }
}

fn send<T>(
    sender: &Option<mpsc::UnboundedSender<T>>,
    command: T,
    kind: ServiceKind,
) -> Result<(), ServiceDispatchError> {
    sender
        .as_ref()
        .ok_or(ServiceDispatchError::Unavailable(kind))?
        .send(command)
        .map_err(|_| ServiceDispatchError::Closed(kind))
}

/// Runtime values which domain services re-read between operations.
#[derive(Clone)]
pub struct RuntimeSettingsPort {
    project_root: PathBuf,
    effort: Arc<AtomicU8>,
    voice_mode: Arc<AtomicBool>,
    context_budget: Arc<AtomicUsize>,
    model: Arc<Mutex<String>>,
    working_dir: Arc<Mutex<PathBuf>>,
    style_plain_language: Arc<AtomicBool>,
    style_target_grade: Arc<AtomicU8>,
}

impl RuntimeSettingsPort {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        project_root: PathBuf,
        effort: Arc<AtomicU8>,
        voice_mode: Arc<AtomicBool>,
        context_budget: Arc<AtomicUsize>,
        model: Arc<Mutex<String>>,
        working_dir: Arc<Mutex<PathBuf>>,
        style_plain_language: Arc<AtomicBool>,
        style_target_grade: Arc<AtomicU8>,
    ) -> Self {
        Self {
            project_root,
            effort,
            voice_mode,
            context_budget,
            model,
            working_dir,
            style_plain_language,
            style_target_grade,
        }
    }

    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    pub fn working_dir(&self) -> PathBuf {
        self.working_dir.lock().unwrap().clone()
    }

    pub fn set_working_dir(&self, path: PathBuf) {
        *self.working_dir.lock().unwrap() = path;
    }

    pub fn set_model(&self, model: String) {
        *self.model.lock().unwrap() = model;
    }

    pub fn apply_visible_effects(
        &self,
        effort: Effort,
        context_budget: usize,
        voice_mode: bool,
        plain_language: bool,
        target_grade: u8,
    ) {
        effort.store(&self.effort);
        self.context_budget.store(context_budget, Ordering::SeqCst);
        self.voice_mode.store(voice_mode, Ordering::SeqCst);
        self.style_plain_language
            .store(plain_language, Ordering::SeqCst);
        self.style_target_grade
            .store(target_grade, Ordering::SeqCst);
    }
}

/// Owns process-private settings while exposing only the browser-safe projection.
pub struct SettingsController {
    project_root: PathBuf,
    settings: Mutex<Settings>,
    model_options: Mutex<HashMap<String, Vec<String>>>,
    runtime: RuntimeSettingsPort,
    selected_backend: Mutex<Option<String>>,
    selected_model: Mutex<Option<String>>,
}

impl SettingsController {
    pub fn new(
        project_root: PathBuf,
        settings: Settings,
        runtime: RuntimeSettingsPort,
        selected_backend: Option<String>,
        selected_model: Option<String>,
    ) -> Self {
        Self {
            project_root,
            settings: Mutex::new(settings),
            model_options: Mutex::new(HashMap::new()),
            runtime,
            selected_backend: Mutex::new(selected_backend),
            selected_model: Mutex::new(selected_model),
        }
    }

    pub fn visible(&self) -> VisibleSettings {
        let mut visible = VisibleSettings::from_settings(
            &self.settings.lock().unwrap(),
            self.selected_backend.lock().unwrap().clone(),
            self.selected_model.lock().unwrap().clone(),
        );
        let model_options = self.model_options.lock().unwrap();
        for backend in &mut visible.backends {
            if let Some(models) = model_options.get(&backend.name) {
                backend.models.clone_from(models);
            }
        }
        visible
    }

    /// Discover every configured backend's selectable models without holding
    /// the settings lock across network or filesystem work.
    pub async fn refresh_models(&self) -> VisibleSettings {
        let backends = self
            .settings
            .lock()
            .unwrap()
            .backends()
            .into_iter()
            .flatten()
            .map(|(name, config)| (name.clone(), config.clone()))
            .collect::<Vec<_>>();
        let discovered = join_all(backends.into_iter().map(|(name, config)| async move {
            let models = list_models(&config).await;
            (name, models)
        }))
        .await
        .into_iter()
        .collect();
        *self.model_options.lock().unwrap() = discovered;
        self.visible()
    }

    pub fn update(&self, visible: VisibleSettings) -> Result<VisibleSettings, AppError> {
        validate_visible(&visible)?;
        let effort = parse_effort(&visible.effort)?;
        let trigger_mode = match visible.voice.trigger_mode.as_str() {
            "push_to_talk" => TriggerMode::PushToTalk,
            "wake_word" => TriggerMode::WakeWord,
            _ => {
                return Err(invalid(
                    "voice.trigger_mode",
                    "unsupported voice trigger mode",
                ));
            }
        };
        let mut stored = self.settings.lock().unwrap();
        let mut settings = stored.clone();
        if let Some(backend_name) = &visible.selected_backend {
            let Some(backend) = settings
                .backends
                .as_mut()
                .and_then(|items| items.get_mut(backend_name))
            else {
                return Err(invalid(
                    "selected_backend",
                    "configured backend does not exist",
                ));
            };
            if let Some(model) = &visible.selected_model {
                set_backend_model(backend, model.clone());
            }
            settings.default_backend = Some(backend_name.clone());
        }
        settings.effort = Some(effort);
        settings.context_budget = Some(visible.context_budget);
        settings.show_raw_output = Some(visible.show_raw_output);
        settings.max_tokens = Some(visible.max_tokens);
        let style = settings.style_mut();
        style.plain_language_enabled = visible.style.plain_language;
        style.target_grade = Some(visible.style.target_grade);
        let voice = settings.voice_mut();
        voice.enabled = visible.voice.enabled;
        voice.stt_enabled = visible.voice.stt_enabled;
        voice.tts_enabled = visible.voice.tts_enabled;
        voice.trigger_mode = trigger_mode;
        voice.wake_phrase = Some(visible.voice.wake_phrase);
        voice.tts_voice = Some(visible.voice.tts_voice);
        voice.tts_speed = Some(visible.voice.tts_speed);
        let procedure = settings.procedure_mut();
        procedure.localization_backend = visible.procedure.localization_backend;
        procedure.local_patch_backend = visible.procedure.local_patch_backend;
        procedure.frontier_patch_backend = visible.procedure.frontier_patch_backend;
        procedure.repository_index.max_files = visible.procedure.index_max_files;
        procedure.repository_index.max_total_bytes = visible.procedure.index_max_total_bytes;
        procedure.verifier_commands = visible.procedure.verifier_commands;
        settings
            .save(&self.project_root)
            .map_err(|error| AppError {
                code: AppErrorCode::PersistenceFailed,
                message: error.to_string(),
                recoverable: true,
                field: None,
            })?;
        if let Some(model) = &visible.selected_model {
            self.runtime.set_model(model.clone());
        }
        self.runtime.apply_visible_effects(
            effort,
            visible.context_budget,
            visible.voice.tts_enabled,
            visible.style.plain_language,
            visible.style.target_grade.round() as u8,
        );
        *self.selected_backend.lock().unwrap() = visible.selected_backend;
        *self.selected_model.lock().unwrap() = visible.selected_model;
        *stored = settings;
        drop(stored);
        Ok(self.visible())
    }

    pub fn set_working_dir(&self, path: PathBuf) -> Result<VisibleSettings, AppError> {
        if !path.is_dir() {
            return Err(invalid("working_dir", "selected path is not a directory"));
        }
        let mut stored = self.settings.lock().unwrap();
        let mut settings = stored.clone();
        let display = path.display().to_string();
        settings.working_dir = Some(display);
        settings
            .save(&self.project_root)
            .map_err(|error| AppError {
                code: AppErrorCode::PersistenceFailed,
                message: error.to_string(),
                recoverable: true,
                field: Some("working_dir".into()),
            })?;
        self.runtime.set_working_dir(path);
        *stored = settings;
        drop(stored);
        Ok(self.visible())
    }

    pub fn working_dir(&self) -> PathBuf {
        self.runtime.working_dir()
    }
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }
}

fn validate_visible(settings: &VisibleSettings) -> Result<(), AppError> {
    if !(32_000..=200_000).contains(&settings.context_budget) {
        return Err(invalid(
            "context_budget",
            "context budget must be between 32000 and 200000",
        ));
    }
    if !(1024..=65_536).contains(&settings.max_tokens) {
        return Err(invalid(
            "max_tokens",
            "max tokens must be between 1024 and 65536",
        ));
    }
    if !(1.0..=20.0).contains(&settings.style.target_grade) {
        return Err(invalid(
            "style.target_grade",
            "target grade must be between 1 and 20",
        ));
    }
    if !(0.5..=2.0).contains(&settings.voice.tts_speed) {
        return Err(invalid(
            "voice.tts_speed",
            "voice speed must be between 0.5 and 2",
        ));
    }
    Ok(())
}

fn parse_effort(value: &str) -> Result<Effort, AppError> {
    match value {
        "none" => Ok(Effort::None),
        "low" => Ok(Effort::Low),
        "medium" => Ok(Effort::Medium),
        "high" => Ok(Effort::High),
        "max" => Ok(Effort::Max),
        _ => Err(invalid("effort", "unsupported effort")),
    }
}

fn set_backend_model(backend: &mut BackendConfig, value: String) {
    match backend {
        BackendConfig::Api { model, .. }
        | BackendConfig::ClaudeCli { model, .. }
        | BackendConfig::CodexCli { model, .. } => *model = value,
    }
}

fn invalid(field: &str, message: &str) -> AppError {
    AppError {
        code: AppErrorCode::InvalidInput,
        message: message.into(),
        recoverable: true,
        field: Some(field.into()),
    }
}
