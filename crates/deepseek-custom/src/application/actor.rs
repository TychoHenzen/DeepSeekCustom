//! Serialized ownership of presentation-neutral application state.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::agent::events::AgentCommand;
use crate::api::types::ImageAttachment;
use crate::application::services::{DomainCommandPort, SettingsController};
use crate::application::session::ApplicationSession;
use crate::gui::PendingSwitch;
use crate::gui::session_state::SessionOrigin;
use crate::gui::transcript::{BlockKind, Severity};
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
            _ => AppCommandResult::Rejected {
                error: unavailable("command is not connected to a domain port yet"),
            },
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
