//! Serialized ownership of presentation-neutral application state.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::agent::events::AgentCommand;
use crate::agent::events::StreamEvent;
use crate::agent::repeat::RepeatCommand;
use crate::api::types::ImageAttachment;
use crate::application::services::{DomainCommandPort, SettingsController};
use crate::application::session::ApplicationSession;
use crate::gui::PendingSwitch;
use crate::gui::session_state::SessionOrigin;
use crate::gui::transcript::{BlockKind, Severity};
use crate::search::cascade::{CascadeParams, MAX_ATTEMPTS};
use crate::search::evolve::{EvolveParams, MAX_TOTAL_DISPATCHES};
use crate::search::{SearchCommand, SearchKind};
use crate::session::SessionId;
use crate::voice::service::VoiceCommand;

use super::dto::{
    AppChange, AppChangeKind, AppCommand, AppCommandRequest, AppCommandResult, AppError,
    AppErrorCode, AppRevision, AppSnapshot, OperationKind, OperationPhase, OperationState,
    PendingSessionSwitch, SessionSummary, TranscriptBlock, TranscriptContent, VisibleSettings,
    Workspace,
};

/// Result of asking the actor for changes after a known revision.
#[derive(Debug, Clone, PartialEq)]
pub enum Replay {
    /// Every change after the requested revision is still retained.
    Changes(Vec<AppChange>),
    /// The requested revision predates retained history. Replace local state.
    Reset(Box<AppSnapshot>),
}

/// A domain or service event already projected into presentation-neutral data.
#[derive(Debug, Clone, PartialEq)]
pub enum AppEvent {
    TranscriptAppended(TranscriptBlock),
    SessionChanged(SessionSummary),
    SavedSessionsChanged(Vec<SessionSummary>),
    PendingSessionSwitchChanged(Option<PendingSessionSwitch>),
    SettingsChanged(VisibleSettings),
    OperationChanged(super::dto::OperationState),
    Error(AppError),
}

/// One mutation owner for commands and domain events.
///
/// Callers need mutable access to submit work. This makes command and event
/// order explicit even before the actor is placed behind its later async loop.
pub struct ApplicationActor {
    snapshot: AppSnapshot,
    replay_capacity: usize,
    changes: VecDeque<AppChange>,
    chat: Option<ChatLifecycle>,
    settings: Option<Arc<SettingsController>>,
    attachments: HashMap<String, ImageAttachment>,
    voice: Option<DomainCommandPort<VoiceCommand>>,
    autopilot: Option<DomainCommandPort<RepeatCommand>>,
    search: Option<DomainCommandPort<SearchCommand>>,
    repeat_interrupt: Option<Arc<AtomicBool>>,
    search_interrupt: Option<Arc<AtomicBool>>,
}

/// Process-private chat lifecycle dependencies used by every presentation adapter.
pub struct ChatLifecycle {
    session: ApplicationSession,
    agent: DomainCommandPort<AgentCommand>,
    interrupt: Arc<AtomicBool>,
    origin: SessionOrigin,
}

impl ChatLifecycle {
    pub fn new(
        session: ApplicationSession,
        agent: DomainCommandPort<AgentCommand>,
        interrupt: Arc<AtomicBool>,
        origin: SessionOrigin,
    ) -> Self {
        Self {
            session,
            agent,
            interrupt,
            origin,
        }
    }
}

impl ApplicationActor {
    pub fn new(mut snapshot: AppSnapshot, replay_capacity: usize) -> Self {
        snapshot.revision = AppRevision::INITIAL;
        Self {
            snapshot,
            replay_capacity,
            changes: VecDeque::with_capacity(replay_capacity),
            chat: None,
            settings: None,
            attachments: HashMap::new(),
            voice: None,
            autopilot: None,
            search: None,
            repeat_interrupt: None,
            search_interrupt: None,
        }
    }

    pub fn with_settings_controller(mut self, settings: Arc<SettingsController>) -> Self {
        self.snapshot.settings = settings.visible();
        self.settings = Some(settings);
        self
    }

    pub fn with_chat_lifecycle(mut self, chat: ChatLifecycle) -> Self {
        self.snapshot.session = chat.session.session_summary();
        self.snapshot.saved_sessions = chat.session.saved_session_summaries();
        self.chat = Some(chat);
        self
    }

    pub fn with_voice_port(mut self, voice: DomainCommandPort<VoiceCommand>) -> Self {
        self.connect_voice_port(voice);
        self
    }

    pub fn connect_voice_port(&mut self, voice: DomainCommandPort<VoiceCommand>) {
        self.voice = Some(voice);
    }

    pub fn connect_autopilot(
        &mut self,
        autopilot: DomainCommandPort<RepeatCommand>,
        interrupt: Arc<AtomicBool>,
    ) {
        self.autopilot = Some(autopilot);
        self.repeat_interrupt = Some(interrupt);
    }

    pub fn connect_search(
        &mut self,
        search: DomainCommandPort<SearchCommand>,
        interrupt: Arc<AtomicBool>,
    ) {
        self.search = Some(search);
        self.search_interrupt = Some(interrupt);
    }

    pub fn register_attachment(&mut self, id: String, attachment: ImageAttachment) {
        self.attachments.insert(id, attachment);
    }

    pub fn remove_attachment(&mut self, id: &str) -> bool {
        self.attachments.remove(id).is_some()
    }

    pub fn snapshot(&self) -> &AppSnapshot {
        &self.snapshot
    }

    pub fn submit(&mut self, request: AppCommandRequest) -> AppCommandResult {
        if request.revision != self.snapshot.revision {
            return AppCommandResult::Conflict {
                current_revision: self.snapshot.revision,
            };
        }
        match request.command {
            AppCommand::SelectWorkspace { workspace } => {
                self.snapshot.workspace = workspace;
                match self.publish(AppChangeKind::WorkspaceSelected(workspace)) {
                    Ok(revision) => AppCommandResult::Applied { revision },
                    Err(error) => AppCommandResult::Rejected { error },
                }
            }
            AppCommand::SendMessage {
                text,
                attachment_id,
            } => {
                let text = text.trim().to_string();
                if text.is_empty() && attachment_id.is_none() {
                    return AppCommandResult::Rejected {
                        error: AppError {
                            code: AppErrorCode::InvalidInput,
                            message: "Enter a message or choose an accepted image.".into(),
                            recoverable: true,
                            field: Some("message".into()),
                        },
                    };
                }
                let image = match attachment_id.as_deref() {
                    Some(id) => match self.attachments.get(id) {
                        Some(image) => Some(image.clone()),
                        None => {
                            return AppCommandResult::Rejected {
                                error: invalid(
                                    "attachment_id",
                                    "attachment is missing or already used",
                                ),
                            };
                        }
                    },
                    None => None,
                };
                if let Some(chat) = &mut self.chat {
                    if chat.session.turn_active {
                        return AppCommandResult::Rejected {
                            error: operation_active("a chat turn is already running"),
                        };
                    }
                    chat.interrupt.store(false, Ordering::SeqCst);
                    if chat
                        .agent
                        .send(AgentCommand::UserTurn {
                            text: text.clone(),
                            image,
                        })
                        .is_err()
                    {
                        return AppCommandResult::Rejected {
                            error: unavailable("agent command channel is closed"),
                        };
                    }
                    chat.session
                        .transcript
                        .push(BlockKind::User { text: text.clone() });
                    chat.session.turn_active = true;
                }
                if let Some(id) = attachment_id.as_deref() {
                    self.attachments.remove(id);
                }
                let id = self
                    .snapshot
                    .transcript
                    .iter()
                    .map(|block| block.id)
                    .max()
                    .unwrap_or(0)
                    + 1;
                let user = TranscriptBlock {
                    id,
                    content: TranscriptContent::User {
                        text,
                        has_image: attachment_id.is_some(),
                    },
                };
                self.snapshot.transcript.push(user.clone());
                if let Err(error) = self.publish(AppChangeKind::TranscriptAppended(user)) {
                    return AppCommandResult::Rejected { error };
                }
                let operation = OperationState {
                    kind: OperationKind::Chat,
                    operation_id: Some(format!("chat-{id}")),
                    phase: OperationPhase::Running,
                    progress: None,
                    message: Some("Generating response".into()),
                    error: None,
                };
                self.snapshot
                    .operations
                    .retain(|item| item.kind != OperationKind::Chat);
                self.snapshot.operations.push(operation.clone());
                match self.publish(AppChangeKind::OperationChanged(operation)) {
                    Ok(revision) => AppCommandResult::Applied { revision },
                    Err(error) => AppCommandResult::Rejected { error },
                }
            }
            AppCommand::StopOperation {
                kind: OperationKind::Chat,
            } => {
                let Some(chat) = &mut self.chat else {
                    return AppCommandResult::Rejected {
                        error: unavailable("chat lifecycle is not connected"),
                    };
                };
                if !chat.session.turn_active {
                    return AppCommandResult::Rejected {
                        error: operation_active("no chat turn is running"),
                    };
                }
                chat.interrupt.store(true, Ordering::SeqCst);
                self.finish_chat(OperationPhase::Interrupted, "Interrupted by user", true)
            }
            AppCommand::StopOperation {
                kind: OperationKind::Autopilot,
            } => self.stop_flagged_operation(OperationKind::Autopilot, false),
            AppCommand::StopOperation {
                kind: kind @ (OperationKind::Cascade | OperationKind::Evolve),
            } => self.stop_flagged_operation(kind, true),
            AppCommand::NewSession => self.request_session_switch(PendingSwitch::New),
            AppCommand::LoadSession { session_id } => {
                let Ok(id) = SessionId::parse(&session_id) else {
                    return AppCommandResult::Rejected {
                        error: invalid("session_id", "session id is invalid"),
                    };
                };
                self.request_session_switch(PendingSwitch::Load(id))
            }
            AppCommand::DeleteSession { session_id } => {
                let Ok(id) = SessionId::parse(&session_id) else {
                    return AppCommandResult::Rejected {
                        error: invalid("session_id", "session id is invalid"),
                    };
                };
                let Some(chat) = &mut self.chat else {
                    return AppCommandResult::Rejected {
                        error: unavailable("chat lifecycle is not connected"),
                    };
                };
                chat.session.sessions.delete(id);
                self.snapshot.saved_sessions = chat.session.saved_session_summaries();
                match self.publish(AppChangeKind::SavedSessionsChanged(
                    self.snapshot.saved_sessions.clone(),
                )) {
                    Ok(revision) => AppCommandResult::Applied { revision },
                    Err(error) => AppCommandResult::Rejected { error },
                }
            }
            AppCommand::UpdateSettings { settings } => {
                let Some(controller) = &self.settings else {
                    return AppCommandResult::Rejected {
                        error: unavailable("settings lifecycle is not connected"),
                    };
                };
                match controller.update(*settings) {
                    Ok(settings) => {
                        if let Some(voice) = &self.voice {
                            for command in [
                                VoiceCommand::SetEnabled(settings.voice.enabled),
                                VoiceCommand::SetSttEnabled(settings.voice.stt_enabled),
                                VoiceCommand::SetTtsEnabled(settings.voice.tts_enabled),
                                VoiceCommand::SetWakePhrase(settings.voice.wake_phrase.clone()),
                                VoiceCommand::SetVoice(settings.voice.tts_voice.clone()),
                                VoiceCommand::SetSpeed(settings.voice.tts_speed),
                            ] {
                                let _ = voice.send(command);
                            }
                        }
                        self.snapshot.settings = settings.clone();
                        match self.publish(AppChangeKind::SettingsChanged(settings)) {
                            Ok(revision) => AppCommandResult::Applied { revision },
                            Err(error) => AppCommandResult::Rejected { error },
                        }
                    }
                    Err(error) => AppCommandResult::Rejected { error },
                }
            }
            AppCommand::StartVoiceCapture => {
                let Some(voice) = &self.voice else {
                    return AppCommandResult::Rejected {
                        error: unavailable("voice service is not connected"),
                    };
                };
                if voice.send(VoiceCommand::StartListening).is_err() {
                    return AppCommandResult::Rejected {
                        error: unavailable("voice command channel is closed"),
                    };
                }
                self.publish_voice_operation("Listening")
            }
            AppCommand::StopVoiceCapture => {
                let Some(voice) = &self.voice else {
                    return AppCommandResult::Rejected {
                        error: unavailable("voice service is not connected"),
                    };
                };
                if voice.send(VoiceCommand::StopListening).is_err() {
                    return AppCommandResult::Rejected {
                        error: unavailable("voice command channel is closed"),
                    };
                }
                self.publish_voice_operation("Transcribing")
            }
            AppCommand::StartAutopilot { task, iterations } => {
                let task = task.trim().to_string();
                if task.is_empty() {
                    return AppCommandResult::Rejected {
                        error: invalid("task", "task is required"),
                    };
                }
                if iterations == 0 {
                    return AppCommandResult::Rejected {
                        error: invalid("iterations", "iterations must be at least 1"),
                    };
                }
                let Some(port) = &self.autopilot else {
                    return AppCommandResult::Rejected {
                        error: unavailable("autopilot service is not connected"),
                    };
                };
                if self.has_active_operation() {
                    return AppCommandResult::Rejected {
                        error: operation_active("another operation is already running"),
                    };
                }
                if let Some(flag) = &self.repeat_interrupt {
                    flag.store(false, Ordering::SeqCst);
                }
                if port.send(RepeatCommand { task, iterations }).is_err() {
                    return AppCommandResult::Rejected {
                        error: unavailable("autopilot command channel is closed"),
                    };
                }
                self.start_operation(
                    OperationKind::Autopilot,
                    Some(iterations.into()),
                    "Autopilot started",
                )
            }
            AppCommand::StartCascade {
                prompt,
                backend,
                n,
                vote_k,
                check_cmd,
                diversity_hints,
                escalate_backend,
            } => {
                if prompt.trim().is_empty() {
                    return AppCommandResult::Rejected {
                        error: invalid("prompt", "prompt is required"),
                    };
                }
                if backend.trim().is_empty() {
                    return AppCommandResult::Rejected {
                        error: invalid("backend", "backend is required"),
                    };
                }
                if n == 0 || n > MAX_ATTEMPTS {
                    return AppCommandResult::Rejected {
                        error: invalid("n", "attempts must be between 1 and 16"),
                    };
                }
                if vote_k == 0 || vote_k > 8 {
                    return AppCommandResult::Rejected {
                        error: invalid("vote_k", "vote margin must be between 1 and 8"),
                    };
                }
                let params = CascadeParams {
                    prompt: prompt.trim().into(),
                    backend,
                    n,
                    vote_k,
                    check_cmd: trimmed_option(check_cmd),
                    diversity_hints: trimmed_lines(diversity_hints),
                    escalate_backend: trimmed_option(escalate_backend),
                    effort: snapshot_effort(&self.snapshot.settings.effort),
                };
                self.start_search(
                    SearchCommand::Cascade(Box::new(params)),
                    OperationKind::Cascade,
                    Some(n.into()),
                )
            }
            AppCommand::StartEvolve {
                prompt,
                backend,
                generations,
                population,
                fitness_cmd,
                feature_cmd,
                islands,
                migration_interval,
                mutation_hints,
            } => {
                if prompt.trim().is_empty() {
                    return AppCommandResult::Rejected {
                        error: invalid("prompt", "prompt is required"),
                    };
                }
                if backend.trim().is_empty() {
                    return AppCommandResult::Rejected {
                        error: invalid("backend", "backend is required"),
                    };
                }
                if fitness_cmd.trim().is_empty() {
                    return AppCommandResult::Rejected {
                        error: invalid("fitness_cmd", "fitness command is required"),
                    };
                }
                if !(1..=50).contains(&generations) {
                    return AppCommandResult::Rejected {
                        error: invalid("generations", "generations must be between 1 and 50"),
                    };
                }
                if !(1..=20).contains(&population) {
                    return AppCommandResult::Rejected {
                        error: invalid("population", "population must be between 1 and 20"),
                    };
                }
                if !(1..=8).contains(&islands) {
                    return AppCommandResult::Rejected {
                        error: invalid("islands", "islands must be between 1 and 8"),
                    };
                }
                if migration_interval > 20 {
                    return AppCommandResult::Rejected {
                        error: invalid(
                            "migration_interval",
                            "migration interval must be between 0 and 20",
                        ),
                    };
                }
                let planned = generations
                    .saturating_mul(population)
                    .saturating_mul(islands);
                let params = EvolveParams {
                    prompt: prompt.trim().into(),
                    backend,
                    generations,
                    population,
                    fitness_cmd: fitness_cmd.trim().into(),
                    feature_cmd: trimmed_option(feature_cmd),
                    islands,
                    migration_interval,
                    mutation_hints: trimmed_lines(mutation_hints),
                    effort: snapshot_effort(&self.snapshot.settings.effort),
                };
                self.start_search(
                    SearchCommand::Evolve(Box::new(params)),
                    OperationKind::Evolve,
                    Some(u64::from(planned.min(MAX_TOTAL_DISPATCHES))),
                )
            }
            _ => AppCommandResult::Rejected {
                error: unavailable("command is not connected to a domain port yet"),
            },
        }
    }

    fn has_active_operation(&self) -> bool {
        self.snapshot.operations.iter().any(|item| {
            matches!(
                item.phase,
                OperationPhase::Running | OperationPhase::AwaitingReview
            )
        })
    }

    fn start_search(
        &mut self,
        command: SearchCommand,
        kind: OperationKind,
        total: Option<u64>,
    ) -> AppCommandResult {
        let Some(port) = &self.search else {
            return AppCommandResult::Rejected {
                error: unavailable("search service is not connected"),
            };
        };
        if self.has_active_operation() {
            return AppCommandResult::Rejected {
                error: operation_active("another operation is already running"),
            };
        }
        if let Some(flag) = &self.search_interrupt {
            flag.store(false, Ordering::SeqCst);
        }
        if port.send(command).is_err() {
            return AppCommandResult::Rejected {
                error: unavailable("search command channel is closed"),
            };
        }
        self.start_operation(kind, total, &format!("{} started", operation_label(kind)))
    }

    fn start_operation(
        &mut self,
        kind: OperationKind,
        total: Option<u64>,
        message: &str,
    ) -> AppCommandResult {
        let operation = OperationState {
            kind,
            operation_id: Some(format!(
                "{}-{}",
                operation_label(kind).to_lowercase(),
                self.snapshot.revision.0 + 1
            )),
            phase: OperationPhase::Running,
            progress: Some(super::dto::OperationProgress {
                completed: 0,
                total,
            }),
            message: Some(message.into()),
            error: None,
        };
        self.snapshot.operations.retain(|item| item.kind != kind);
        self.snapshot.operations.push(operation.clone());
        match self.publish(AppChangeKind::OperationChanged(operation)) {
            Ok(revision) => AppCommandResult::Applied { revision },
            Err(error) => AppCommandResult::Rejected { error },
        }
    }

    fn stop_flagged_operation(&mut self, kind: OperationKind, search: bool) -> AppCommandResult {
        let running = self
            .snapshot
            .operations
            .iter()
            .any(|item| item.kind == kind && item.phase == OperationPhase::Running);
        if !running {
            return AppCommandResult::Rejected {
                error: operation_active("operation is not running"),
            };
        }
        let flag = if search {
            &self.search_interrupt
        } else {
            &self.repeat_interrupt
        };
        let Some(flag) = flag else {
            return AppCommandResult::Rejected {
                error: unavailable("operation stop flag is not connected"),
            };
        };
        flag.store(true, Ordering::SeqCst);
        match self.publish(AppChangeKind::OperationChanged(
            self.snapshot
                .operations
                .iter()
                .find(|item| item.kind == kind)
                .unwrap()
                .clone(),
        )) {
            Ok(revision) => AppCommandResult::Applied { revision },
            Err(error) => AppCommandResult::Rejected { error },
        }
    }

    fn publish_voice_operation(&mut self, message: &str) -> AppCommandResult {
        let operation = OperationState {
            kind: OperationKind::Voice,
            operation_id: Some("voice-capture".into()),
            phase: OperationPhase::Running,
            progress: None,
            message: Some(message.to_string()),
            error: None,
        };
        self.snapshot
            .operations
            .retain(|item| item.kind != OperationKind::Voice);
        self.snapshot.operations.push(operation.clone());
        match self.publish(AppChangeKind::OperationChanged(operation)) {
            Ok(revision) => AppCommandResult::Applied { revision },
            Err(error) => AppCommandResult::Rejected { error },
        }
    }

    pub fn apply_event(&mut self, event: AppEvent) -> Result<AppRevision, AppError> {
        if let AppEvent::TranscriptAppended(TranscriptBlock {
            content: TranscriptContent::Terminal { outcome, message },
            ..
        }) = &event
            && self
                .chat
                .as_ref()
                .is_some_and(|chat| chat.session.turn_active)
        {
            return match self.finish_chat(*outcome, message, true) {
                AppCommandResult::Applied { revision } => Ok(revision),
                AppCommandResult::Rejected { error } => Err(error),
                AppCommandResult::Conflict { .. } => unreachable!(),
            };
        }
        let change = match event {
            AppEvent::TranscriptAppended(block) => {
                self.snapshot.transcript.push(block.clone());
                AppChangeKind::TranscriptAppended(block)
            }
            AppEvent::SessionChanged(session) => {
                self.snapshot.session = session.clone();
                AppChangeKind::SessionChanged(session)
            }
            AppEvent::SavedSessionsChanged(sessions) => {
                self.snapshot.saved_sessions = sessions.clone();
                AppChangeKind::SavedSessionsChanged(sessions)
            }
            AppEvent::PendingSessionSwitchChanged(pending) => {
                self.snapshot.pending_session_switch = pending.clone();
                AppChangeKind::PendingSessionSwitchChanged(pending)
            }
            AppEvent::SettingsChanged(settings) => {
                self.snapshot.settings = settings.clone();
                AppChangeKind::SettingsChanged(settings)
            }
            AppEvent::OperationChanged(operation) => {
                if let Some(existing) = self
                    .snapshot
                    .operations
                    .iter_mut()
                    .find(|existing| existing.kind == operation.kind)
                {
                    *existing = operation.clone();
                } else {
                    self.snapshot.operations.push(operation.clone());
                }
                AppChangeKind::OperationChanged(operation)
            }
            AppEvent::Error(error) => AppChangeKind::Error(error),
        };
        self.publish(change)
    }

    /// Project existing backend-neutral repeat and search events into browser state.
    pub fn apply_operation_stream_event(
        &mut self,
        event: &StreamEvent,
    ) -> Option<Result<AppRevision, AppError>> {
        let state = match event {
            StreamEvent::RepeatIterationStart { index, total, .. } => OperationState {
                kind: OperationKind::Autopilot,
                operation_id: self.operation_id(OperationKind::Autopilot),
                phase: OperationPhase::Running,
                progress: Some(super::dto::OperationProgress {
                    completed: u64::from(index.saturating_sub(1)),
                    total: Some(u64::from(*total)),
                }),
                message: Some(format!("Running iteration {index} of {total}")),
                error: None,
            },
            StreamEvent::RepeatFinished { completed, total } => OperationState {
                kind: OperationKind::Autopilot,
                operation_id: self.operation_id(OperationKind::Autopilot),
                phase: if *completed < *total
                    && self
                        .repeat_interrupt
                        .as_ref()
                        .is_some_and(|flag| flag.load(Ordering::SeqCst))
                {
                    OperationPhase::Interrupted
                } else {
                    OperationPhase::Completed
                },
                progress: Some(super::dto::OperationProgress {
                    completed: u64::from(*completed),
                    total: Some(u64::from(*total)),
                }),
                message: Some(format!(
                    "Autopilot finished: {completed} of {total} iterations"
                )),
                error: None,
            },
            StreamEvent::SearchProgress(snapshot) => OperationState {
                kind: operation_kind(snapshot.kind),
                operation_id: self.operation_id(operation_kind(snapshot.kind)),
                phase: OperationPhase::Running,
                progress: Some(super::dto::OperationProgress {
                    completed: u64::from(snapshot.done),
                    total: Some(u64::from(snapshot.total)),
                }),
                message: Some(search_message(snapshot)),
                error: None,
            },
            StreamEvent::SearchFinished {
                kind,
                summary,
                is_error,
            } => OperationState {
                kind: operation_kind(*kind),
                operation_id: self.operation_id(operation_kind(*kind)),
                phase: if self
                    .search_interrupt
                    .as_ref()
                    .is_some_and(|flag| flag.load(Ordering::SeqCst))
                {
                    OperationPhase::Interrupted
                } else if *is_error {
                    OperationPhase::Failed
                } else {
                    OperationPhase::Completed
                },
                progress: self
                    .snapshot
                    .operations
                    .iter()
                    .find(|item| item.kind == operation_kind(*kind))
                    .and_then(|item| item.progress),
                message: Some(summary.clone()),
                error: is_error.then(|| AppError {
                    code: AppErrorCode::ServiceFailed,
                    message: summary.clone(),
                    recoverable: true,
                    field: None,
                }),
            },
            _ => return None,
        };
        Some(self.apply_event(AppEvent::OperationChanged(state)))
    }

    fn operation_id(&self, kind: OperationKind) -> Option<String> {
        self.snapshot
            .operations
            .iter()
            .find(|item| item.kind == kind)
            .and_then(|item| item.operation_id.clone())
    }

    pub fn replay_after(&self, revision: AppRevision) -> Replay {
        if revision > self.snapshot.revision {
            return Replay::Reset(Box::new(self.snapshot.clone()));
        }
        if revision == self.snapshot.revision {
            return Replay::Changes(Vec::new());
        }
        let oldest_base = self
            .changes
            .front()
            .map(|change| AppRevision(change.revision.0.saturating_sub(1)))
            .unwrap_or(self.snapshot.revision);
        if revision < oldest_base {
            return Replay::Reset(Box::new(self.snapshot.clone()));
        }
        Replay::Changes(
            self.changes
                .iter()
                .filter(|change| change.revision > revision)
                .cloned()
                .collect(),
        )
    }

    fn publish(&mut self, change: AppChangeKind) -> Result<AppRevision, AppError> {
        let revision = self
            .snapshot
            .revision
            .checked_next()
            .ok_or_else(|| AppError {
                code: AppErrorCode::ServiceFailed,
                message: "application revision exhausted".to_string(),
                recoverable: false,
                field: None,
            })?;
        self.snapshot.revision = revision;
        if self.replay_capacity > 0 {
            if self.changes.len() == self.replay_capacity {
                self.changes.pop_front();
            }
            self.changes.push_back(AppChange { revision, change });
        }
        Ok(revision)
    }

    fn request_session_switch(&mut self, pending: PendingSwitch) -> AppCommandResult {
        let Some(chat) = &mut self.chat else {
            return AppCommandResult::Rejected {
                error: unavailable("chat lifecycle is not connected"),
            };
        };
        if chat.session.turn_active {
            chat.session.pending_switch = Some(pending.clone());
            let projected = chat.session.pending_session_switch();
            self.snapshot.pending_session_switch = projected.clone();
            return match self.publish(AppChangeKind::PendingSessionSwitchChanged(projected)) {
                Ok(revision) => AppCommandResult::Applied { revision },
                Err(error) => AppCommandResult::Rejected { error },
            };
        }
        self.apply_session_switch(pending)
    }

    fn apply_session_switch(&mut self, pending: PendingSwitch) -> AppCommandResult {
        let Some(chat) = &mut self.chat else {
            unreachable!()
        };
        let command = match pending {
            PendingSwitch::New => Some(
                chat.session
                    .sessions
                    .start_new(&mut chat.session.transcript, chat.origin.clone()),
            ),
            PendingSwitch::Load(id) => {
                chat.session
                    .sessions
                    .load(id, &mut chat.session.transcript, chat.origin.clone())
            }
        };
        let Some(command) = command else {
            return AppCommandResult::Rejected {
                error: not_found("saved session was not found"),
            };
        };
        if chat.agent.send(command).is_err() {
            return AppCommandResult::Rejected {
                error: unavailable("agent command channel is closed"),
            };
        }
        self.snapshot.transcript = chat.session.transcript_projection();
        self.snapshot.session = chat.session.session_summary();
        self.snapshot.saved_sessions = chat.session.saved_session_summaries();
        self.snapshot.pending_session_switch = None;
        let mut reset = self.snapshot.clone();
        reset.revision = match self.snapshot.revision.checked_next() {
            Some(revision) => revision,
            None => {
                return AppCommandResult::Rejected {
                    error: unavailable("application revision exhausted"),
                };
            }
        };
        match self.publish(AppChangeKind::Reset(Box::new(reset))) {
            Ok(revision) => AppCommandResult::Applied { revision },
            Err(error) => AppCommandResult::Rejected { error },
        }
    }

    fn finish_chat(
        &mut self,
        phase: OperationPhase,
        message: &str,
        append_terminal: bool,
    ) -> AppCommandResult {
        if append_terminal {
            let terminal = TranscriptBlock {
                id: self
                    .snapshot
                    .transcript
                    .iter()
                    .map(|block| block.id)
                    .max()
                    .unwrap_or(0)
                    + 1,
                content: TranscriptContent::Terminal {
                    outcome: phase,
                    message: message.into(),
                },
            };
            self.snapshot.transcript.push(terminal.clone());
            if self
                .publish(AppChangeKind::TranscriptAppended(terminal))
                .is_err()
            {
                return AppCommandResult::Rejected {
                    error: unavailable("could not publish terminal event"),
                };
            }
        }
        let pending = {
            let chat = self.chat.as_mut().expect("checked by caller");
            chat.session.transcript.push(BlockKind::Notice {
                text: message.into(),
                severity: Severity::Info,
            });
            chat.session.turn_active = false;
            chat.session
                .sessions
                .autosave(&mut chat.session.transcript, chat.origin.clone());
            chat.session.pending_switch.take()
        };
        let operation = OperationState {
            kind: OperationKind::Chat,
            operation_id: self
                .snapshot
                .operations
                .iter()
                .find(|item| item.kind == OperationKind::Chat)
                .and_then(|item| item.operation_id.clone()),
            phase,
            progress: None,
            message: Some(message.into()),
            error: None,
        };
        self.snapshot
            .operations
            .retain(|item| item.kind != OperationKind::Chat);
        self.snapshot.operations.push(operation.clone());
        let revision = match self.publish(AppChangeKind::OperationChanged(operation)) {
            Ok(revision) => revision,
            Err(error) => return AppCommandResult::Rejected { error },
        };
        if let Some(pending) = pending {
            return self.apply_session_switch(pending);
        }
        AppCommandResult::Applied { revision }
    }
}

fn unavailable(message: &str) -> AppError {
    AppError {
        code: AppErrorCode::Unavailable,
        message: message.to_string(),
        recoverable: true,
        field: None,
    }
}

fn operation_active(message: &str) -> AppError {
    AppError {
        code: AppErrorCode::OperationActive,
        message: message.into(),
        recoverable: true,
        field: None,
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

fn not_found(message: &str) -> AppError {
    AppError {
        code: AppErrorCode::NotFound,
        message: message.into(),
        recoverable: true,
        field: None,
    }
}

fn trimmed_option(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

fn trimmed_lines(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .filter_map(|value| trimmed_option(Some(value)))
        .collect()
}

fn snapshot_effort(value: &str) -> crate::effort::Effort {
    match value {
        "low" => crate::effort::Effort::Low,
        "medium" => crate::effort::Effort::Medium,
        "high" => crate::effort::Effort::High,
        "max" => crate::effort::Effort::Max,
        _ => crate::effort::Effort::None,
    }
}

fn operation_kind(kind: SearchKind) -> OperationKind {
    match kind {
        SearchKind::Cascade => OperationKind::Cascade,
        SearchKind::Evolve => OperationKind::Evolve,
    }
}

fn operation_label(kind: OperationKind) -> &'static str {
    match kind {
        OperationKind::Autopilot => "Autopilot",
        OperationKind::Cascade => "Cascade",
        OperationKind::Evolve => "Evolve",
        _ => "Operation",
    }
}

fn search_message(snapshot: &crate::search::SearchSnapshot) -> String {
    let mut message = snapshot.note.clone();
    if let Some((used, limit)) = snapshot.dispatches {
        message.push_str(&format!("\nDispatches: {used} of {limit}"));
    }
    for entry in &snapshot.top {
        let score = entry
            .score
            .map(|score| format!(" ({score:.4})"))
            .unwrap_or_default();
        message.push_str(&format!("\n{}{}: {}", entry.label, score, entry.preview));
    }
    message
}

impl AppSnapshot {
    /// Minimal actor state for adapters that have not projected all services yet.
    pub fn initial(settings: VisibleSettings, session: SessionSummary) -> Self {
        Self {
            revision: AppRevision::INITIAL,
            workspace: Workspace::Chat,
            transcript: Vec::new(),
            session,
            saved_sessions: Vec::new(),
            pending_session_switch: None,
            settings,
            operations: Vec::new(),
        }
    }
}
