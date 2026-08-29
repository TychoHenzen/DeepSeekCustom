//! Typed, presentation-neutral ports to long-lived domain services.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use crate::agent::events::AgentCommand;
use crate::agent::repeat::RepeatCommand;
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

    pub fn with_search(mut self, sender: mpsc::UnboundedSender<SearchCommand>) -> Self {
        self.search = Some(sender);
        self
    }

    pub fn with_procedure(mut self, sender: mpsc::UnboundedSender<ProcedureCommand>) -> Self {
        self.procedure = Some(sender);
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
