//! Serialized ownership of presentation-neutral application state.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::session::PendingSwitch;
use super::session_state::SessionOrigin;
use super::transcript::{BlockKind, Severity};
use crate::agent::events::{AgentCommand, RoutedEvent, StreamEvent};
use crate::agent::repeat::RepeatCommand;
use crate::api::types::ImageAttachment;
use crate::application::services::{DomainCommandPort, SettingsController};
use crate::application::session::ApplicationSession;
use crate::procedure::{
    ProcedureCommand, ProcedureProgress, ProcedureTerminalDisposition, RouteOverride,
};
use crate::search::{SearchCommand, SearchKind};
use crate::voice::service::VoiceCommand;

use super::dto::{
    AppChange, AppChangeKind, AppCommandRequest, AppCommandResult, AppError, AppErrorCode,
    AppRevision, AppSnapshot, OperationKind, OperationPhase, OperationState, PendingSessionSwitch,
    ProcedureRouteOverride, SessionSummary, TranscriptBlock, TranscriptContent, VisibleSettings,
    Workspace,
};

impl From<ProcedureRouteOverride> for RouteOverride {
    fn from(value: ProcedureRouteOverride) -> Self {
        match value {
            ProcedureRouteOverride::Automatic => Self::Automatic,
            ProcedureRouteOverride::ForceLocal => Self::ForceLocal,
            ProcedureRouteOverride::ForceFrontier => Self::ForceFrontier,
        }
    }
}

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
    TestsChanged(Box<super::test_control::TestControlSnapshot>),
    Error(AppError),
}

/// One mutation owner for commands and domain events.
///
/// Callers need mutable access to submit work. This makes command and event
/// order explicit even before the actor is placed behind its later async loop.
pub struct ApplicationActor {
    pub(super) snapshot: AppSnapshot,
    replay_capacity: usize,
    changes: VecDeque<AppChange>,
    pub(super) chat: Option<ChatLifecycle>,
    pub(super) settings: Option<Arc<SettingsController>>,
    pub(super) attachments: HashMap<String, ImageAttachment>,
    pub(super) voice: Option<DomainCommandPort<VoiceCommand>>,
    pub(super) autopilot: Option<DomainCommandPort<RepeatCommand>>,
    pub(super) search: Option<DomainCommandPort<SearchCommand>>,
    pub(super) procedure: Option<DomainCommandPort<ProcedureCommand>>,
    pub(super) repeat_interrupt: Option<Arc<AtomicBool>>,
    pub(super) search_interrupt: Option<Arc<AtomicBool>>,
    pub(super) procedure_interrupt: Option<Arc<AtomicBool>>,
}

/// Process-private chat lifecycle dependencies used by every presentation adapter.
pub struct ChatLifecycle {
    pub(super) session: ApplicationSession,
    pub(super) agent: DomainCommandPort<AgentCommand>,
    pub(super) interrupt: Arc<AtomicBool>,
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
            procedure: None,
            repeat_interrupt: None,
            search_interrupt: None,
            procedure_interrupt: None,
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

    pub fn connect_chat_lifecycle(&mut self, chat: ChatLifecycle) {
        self.snapshot.session = chat.session.session_summary();
        self.snapshot.saved_sessions = chat.session.saved_session_summaries();
        self.chat = Some(chat);
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

    pub fn connect_procedure(
        &mut self,
        procedure: DomainCommandPort<ProcedureCommand>,
        interrupt: Arc<AtomicBool>,
    ) {
        self.procedure = Some(procedure);
        self.procedure_interrupt = Some(interrupt);
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

    /// Compatibility entry point for direct actor integrations.
    pub fn submit(&mut self, request: AppCommandRequest) -> AppCommandResult {
        super::command_dispatcher::ApplicationCommandDispatcher::new(self).dispatch(request)
    }

    pub(super) fn has_active_operation(&self) -> bool {
        self.snapshot.operations.iter().any(|item| {
            matches!(
                item.phase,
                OperationPhase::Running | OperationPhase::AwaitingReview
            )
        })
    }

    pub(super) fn start_search(
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

    pub(super) fn start_operation(
        &mut self,
        kind: OperationKind,
        total: Option<u64>,
        message: &str,
    ) -> AppCommandResult {
        self.start_operation_with_id(
            kind,
            Some(format!(
                "{}-{}",
                operation_label(kind).to_lowercase(),
                self.snapshot.revision.0 + 1
            )),
            total,
            message,
        )
    }

    pub(super) fn start_operation_with_id(
        &mut self,
        kind: OperationKind,
        operation_id: Option<String>,
        total: Option<u64>,
        message: &str,
    ) -> AppCommandResult {
        let operation = OperationState {
            kind,
            operation_id,
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

    pub(super) fn start_procedure_command(
        &mut self,
        operation_id: String,
        message: &str,
    ) -> AppCommandResult {
        if let Some(flag) = &self.procedure_interrupt {
            flag.store(false, Ordering::SeqCst);
        }
        self.start_operation_with_id(OperationKind::Procedure, Some(operation_id), None, message)
    }

    pub(super) fn stop_procedure(&mut self) -> AppCommandResult {
        let running = self.snapshot.operations.iter().any(|item| {
            item.kind == OperationKind::Procedure && item.phase == OperationPhase::Running
        });
        if !running {
            return AppCommandResult::Rejected {
                error: operation_active("operation is not running"),
            };
        }
        let Some(flag) = &self.procedure_interrupt else {
            return AppCommandResult::Rejected {
                error: unavailable("procedure stop flag is not connected"),
            };
        };
        flag.store(true, Ordering::SeqCst);
        let operation = self
            .snapshot
            .operations
            .iter()
            .find(|item| item.kind == OperationKind::Procedure)
            .unwrap()
            .clone();
        match self.publish(AppChangeKind::OperationChanged(operation)) {
            Ok(revision) => AppCommandResult::Applied { revision },
            Err(error) => AppCommandResult::Rejected { error },
        }
    }

    pub(super) fn stop_flagged_operation(
        &mut self,
        kind: OperationKind,
        search: bool,
    ) -> AppCommandResult {
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

    pub(super) fn publish_voice_operation(&mut self, message: &str) -> AppCommandResult {
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
            AppEvent::TestsChanged(tests) => {
                self.snapshot.tests = tests.as_ref().clone();
                AppChangeKind::TestsChanged(tests)
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

    /// Apply one backend event to the browser-visible transcript and lifecycle.
    ///
    /// Repeat and search events keep their operation projection above. Chat and
    /// routed subagent events also update the actor-owned transcript. A reset is
    /// published because streamed deltas can update the last visible block rather
    /// than only append a new block.
    pub fn apply_routed_stream_event(
        &mut self,
        routed: RoutedEvent,
    ) -> Option<Result<AppRevision, AppError>> {
        let operation_result = self.apply_operation_stream_event(&routed.event);
        let main_session = routed.route.is_empty();

        if main_session {
            match &routed.event {
                StreamEvent::ConversationSnapshot {
                    messages,
                    claude_session_id,
                } => {
                    if let Some(chat) = &mut self.chat {
                        chat.session
                            .sessions
                            .record_snapshot(messages, claude_session_id);
                    }
                    return operation_result;
                }
                StreamEvent::SessionReset => return Some(self.apply_agent_session_reset()),
                StreamEvent::SearchProgress(_) => return operation_result,
                _ => {}
            }
        }

        if self.chat.is_none() {
            return operation_result;
        }
        let terminal_event = routed.event.clone();
        if let Some(chat) = &mut self.chat {
            chat.session.transcript.apply_routed_event(routed);
        }
        let transcript_result = self.publish_chat_transcript_reset();

        if !main_session {
            return Some(transcript_result);
        }
        if let Err(error) = transcript_result {
            return Some(Err(error));
        }

        match terminal_event {
            StreamEvent::TurnEnd { finish_reason, .. }
                if self
                    .chat
                    .as_ref()
                    .is_some_and(|chat| chat.session.turn_active) =>
            {
                let (phase, message) = if finish_reason == "error" {
                    (OperationPhase::Failed, "Turn failed")
                } else {
                    (OperationPhase::Completed, "Response complete")
                };
                Some(command_result(self.finish_chat(phase, message, true)))
            }
            StreamEvent::Interrupted { ref message }
                if self
                    .chat
                    .as_ref()
                    .is_some_and(|chat| chat.session.turn_active) =>
            {
                Some(command_result(self.finish_chat(
                    OperationPhase::Interrupted,
                    message,
                    true,
                )))
            }
            _ => Some(Ok(self.snapshot.revision)),
        }
    }

    /// Project run-scoped Procedure progress without allowing stale runs to
    /// replace the evidence or decisions for the currently displayed run.
    pub fn apply_procedure_progress(
        &mut self,
        event: &ProcedureProgress,
    ) -> Option<Result<AppRevision, AppError>> {
        match event {
            ProcedureProgress::PreviewStarted { preview_id } => {
                return Some(self.apply_procedure_mode_state(
                    preview_id.as_str(),
                    OperationPhase::Running,
                    "Patch preview started".into(),
                    None,
                ));
            }
            ProcedureProgress::PreviewFinished {
                preview_id,
                preview,
                report_path,
            } => {
                let evidence = format!(
                    "Patch preview completed\nAutomatic route: {}\nOverride: {}\nEffective route: {}\nBackend: {}\nModel: {}\nTargets:\n{}\nRationale: {}\nReport: {}\nComplete unified diff:\n{}",
                    preview.route.automatic_tier,
                    preview.route.selected_override,
                    preview.route.effective_tier,
                    preview.backend,
                    preview.model,
                    preview.targets.join("\n"),
                    preview.rationale,
                    report_path.display(),
                    preview.unified_diff,
                );
                return Some(self.apply_procedure_mode_state(
                    preview_id.as_str(),
                    OperationPhase::Completed,
                    evidence,
                    None,
                ));
            }
            ProcedureProgress::PreviewFailed {
                preview_id,
                message,
            } => {
                return Some(self.apply_procedure_mode_state(
                    preview_id.as_str(),
                    OperationPhase::Failed,
                    format!("Patch preview failed: {message}"),
                    Some(AppError {
                        code: AppErrorCode::ServiceFailed,
                        message: message.clone(),
                        recoverable: true,
                        field: None,
                    }),
                ));
            }
            ProcedureProgress::SampledFinished {
                run_id,
                disposition,
                message,
            } => {
                let (phase, error) = terminal_procedure_state(disposition);
                return Some(self.apply_procedure_mode_state(
                    run_id.as_str(),
                    phase,
                    format!("{message}\nTerminal disposition: {disposition:?}"),
                    error,
                ));
            }
            ProcedureProgress::Apply { run_id, progress } => {
                let (phase, error) = match progress.as_ref() {
                    crate::procedure::ProcedureApplyProgress::Finished { disposition } => {
                        terminal_procedure_state(disposition)
                    }
                    crate::procedure::ProcedureApplyProgress::PromotionFailed {
                        message, ..
                    } => (
                        OperationPhase::Failed,
                        Some(AppError {
                            code: AppErrorCode::ServiceFailed,
                            message: message.clone(),
                            recoverable: true,
                            field: None,
                        }),
                    ),
                    _ => (OperationPhase::Running, None),
                };
                return Some(self.apply_procedure_mode_state(
                    run_id.as_str(),
                    phase,
                    format!("Apply evidence: {progress:#?}"),
                    error,
                ));
            }
            ProcedureProgress::RepairTransition { run_id, event } => {
                return Some(self.apply_procedure_mode_state(
                    run_id.as_str(),
                    OperationPhase::Running,
                    format!("Repair transition: {event:#?}"),
                    None,
                ));
            }
            _ => {}
        }
        let (run_id, phase, progress, evidence, error) = match event {
            ProcedureProgress::RunStarted {
                run_id,
                change_id,
                task_id,
            } => (
                *run_id,
                OperationPhase::Running,
                Some((0, Some(3))),
                format!("Run started\nChange: {change_id}\nTask: {task_id}"),
                None,
            ),
            ProcedureProgress::StageStarted { run_id, stage } => (
                *run_id,
                OperationPhase::Running,
                None,
                format!("Stage started: {stage:?}"),
                None,
            ),
            ProcedureProgress::StageCompleted { run_id, stage } => (
                *run_id,
                OperationPhase::Running,
                None,
                format!("Stage completed: {stage:?}"),
                None,
            ),
            ProcedureProgress::AttemptStarted {
                run_id,
                number,
                backend,
                model,
            } => (
                *run_id,
                OperationPhase::Running,
                None,
                format!("Attempt {number}: {backend} / {model}"),
                None,
            ),
            ProcedureProgress::AttemptRejected {
                run_id,
                number,
                error,
            } => (
                *run_id,
                OperationPhase::Running,
                None,
                format!("Attempt {number} rejected: {error}"),
                None,
            ),
            ProcedureProgress::AttemptAccepted {
                run_id,
                number,
                targets,
            } => (
                *run_id,
                OperationPhase::Running,
                None,
                format!("Attempt {number} accepted with {targets} target(s)"),
                None,
            ),
            ProcedureProgress::RunFinished {
                run_id,
                disposition,
            } => {
                let (phase, error) = match disposition {
                    ProcedureTerminalDisposition::Succeeded => (OperationPhase::Completed, None),
                    ProcedureTerminalDisposition::AwaitingReview => {
                        (OperationPhase::AwaitingReview, None)
                    }
                    ProcedureTerminalDisposition::Interrupted => {
                        (OperationPhase::Interrupted, None)
                    }
                    ProcedureTerminalDisposition::Failed { reason } => (
                        OperationPhase::Failed,
                        Some(AppError {
                            code: AppErrorCode::ServiceFailed,
                            message: reason.clone(),
                            recoverable: true,
                            field: None,
                        }),
                    ),
                };
                (
                    *run_id,
                    phase,
                    Some((3, Some(3))),
                    format!("Terminal disposition: {disposition:?}"),
                    error,
                )
            }
            ProcedureProgress::ReviewSucceeded {
                run_id,
                disposition,
            } => (
                *run_id,
                OperationPhase::Completed,
                None,
                format!("Review decision: {disposition}"),
                None,
            ),
            ProcedureProgress::ReviewFailed {
                run_id,
                disposition,
                error,
            } => (
                *run_id,
                OperationPhase::AwaitingReview,
                None,
                format!("Review {disposition} failed: {error}"),
                Some(AppError {
                    code: AppErrorCode::ServiceFailed,
                    message: error.clone(),
                    recoverable: true,
                    field: None,
                }),
            ),
            ProcedureProgress::RunFailed { run_id, message } => (
                *run_id,
                OperationPhase::Failed,
                None,
                format!("Run failed: {message}"),
                Some(AppError {
                    code: AppErrorCode::ServiceFailed,
                    message: message.clone(),
                    recoverable: true,
                    field: None,
                }),
            ),
            _ => return None,
        };
        let run_id = run_id.as_str();
        let previous = self
            .snapshot
            .operations
            .iter()
            .find(|operation| operation.kind == OperationKind::Procedure);
        if !matches!(event, ProcedureProgress::RunStarted { .. })
            && previous.and_then(|operation| operation.operation_id.as_deref())
                != Some(run_id.as_str())
        {
            return None;
        }
        let message = previous
            .and_then(|operation| operation.message.as_deref())
            .map_or(evidence.clone(), |current| format!("{current}\n{evidence}"));
        let progress = progress
            .map(|(completed, total)| super::dto::OperationProgress { completed, total })
            .or_else(|| previous.and_then(|operation| operation.progress));
        Some(self.apply_event(AppEvent::OperationChanged(OperationState {
            kind: OperationKind::Procedure,
            operation_id: Some(run_id),
            phase,
            progress,
            message: Some(message),
            error,
        })))
    }

    fn apply_procedure_mode_state(
        &mut self,
        operation_id: String,
        phase: OperationPhase,
        evidence: String,
        error: Option<AppError>,
    ) -> Result<AppRevision, AppError> {
        let previous = self.snapshot.operations.iter().find(|operation| {
            operation.kind == OperationKind::Procedure
                && operation.operation_id.as_deref() == Some(operation_id.as_str())
        });
        let message = previous
            .and_then(|operation| operation.message.as_deref())
            .map_or(evidence.clone(), |current| format!("{current}\n{evidence}"));
        self.apply_event(AppEvent::OperationChanged(OperationState {
            kind: OperationKind::Procedure,
            operation_id: Some(operation_id),
            phase,
            progress: previous.and_then(|operation| operation.progress),
            message: Some(message),
            error,
        }))
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

    pub(super) fn publish(&mut self, change: AppChangeKind) -> Result<AppRevision, AppError> {
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

    pub(super) fn request_session_switch(&mut self, pending: PendingSwitch) -> AppCommandResult {
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

    fn publish_chat_transcript_reset(&mut self) -> Result<AppRevision, AppError> {
        let mut image_flags = self
            .snapshot
            .transcript
            .iter()
            .filter_map(|block| match &block.content {
                TranscriptContent::User { has_image, .. } => Some(*has_image),
                _ => None,
            })
            .collect::<VecDeque<_>>();
        let mut transcript = self
            .chat
            .as_ref()
            .expect("chat lifecycle checked by caller")
            .session
            .transcript_projection();
        for block in &mut transcript {
            if let TranscriptContent::User { has_image, .. } = &mut block.content {
                *has_image = image_flags.pop_front().unwrap_or(false);
            }
        }
        self.snapshot.transcript = transcript;
        let mut reset = self.snapshot.clone();
        reset.revision = self
            .snapshot
            .revision
            .checked_next()
            .ok_or_else(|| unavailable("application revision exhausted"))?;
        self.publish(AppChangeKind::Reset(Box::new(reset)))
    }

    fn apply_agent_session_reset(&mut self) -> Result<AppRevision, AppError> {
        let Some(chat) = &mut self.chat else {
            return Err(unavailable("chat lifecycle is not connected"));
        };
        chat.session
            .sessions
            .save_outgoing_and_start_new(&mut chat.session.transcript, chat.origin.clone());
        self.snapshot.transcript = chat.session.transcript_projection();
        self.snapshot.session = chat.session.session_summary();
        self.snapshot.saved_sessions = chat.session.saved_session_summaries();
        self.snapshot.pending_session_switch = None;
        let mut reset = self.snapshot.clone();
        reset.revision = self
            .snapshot
            .revision
            .checked_next()
            .ok_or_else(|| unavailable("application revision exhausted"))?;
        self.publish(AppChangeKind::Reset(Box::new(reset)))
    }

    pub(super) fn finish_chat(
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

fn command_result(result: AppCommandResult) -> Result<AppRevision, AppError> {
    match result {
        AppCommandResult::Applied { revision } => Ok(revision),
        AppCommandResult::Rejected { error } => Err(error),
        AppCommandResult::Conflict { .. } => unreachable!("internal events cannot conflict"),
    }
}

pub(super) fn unavailable(message: &str) -> AppError {
    AppError {
        code: AppErrorCode::Unavailable,
        message: message.to_string(),
        recoverable: true,
        field: None,
    }
}

pub(super) fn operation_active(message: &str) -> AppError {
    AppError {
        code: AppErrorCode::OperationActive,
        message: message.into(),
        recoverable: true,
        field: None,
    }
}

pub(super) fn invalid(field: &str, message: &str) -> AppError {
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

pub(super) fn trimmed_option(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

pub(super) fn trimmed_lines(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .filter_map(|value| trimmed_option(Some(value)))
        .collect()
}

pub(super) fn snapshot_effort(value: &str) -> crate::effort::Effort {
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

fn terminal_procedure_state(
    disposition: &ProcedureTerminalDisposition,
) -> (OperationPhase, Option<AppError>) {
    match disposition {
        ProcedureTerminalDisposition::Succeeded => (OperationPhase::Completed, None),
        ProcedureTerminalDisposition::AwaitingReview => (OperationPhase::AwaitingReview, None),
        ProcedureTerminalDisposition::Interrupted => (OperationPhase::Interrupted, None),
        ProcedureTerminalDisposition::Failed { reason } => (
            OperationPhase::Failed,
            Some(AppError {
                code: AppErrorCode::ServiceFailed,
                message: reason.clone(),
                recoverable: true,
                field: None,
            }),
        ),
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
            tests: Default::default(),
        }
    }
}
