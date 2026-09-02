//! Serialized ownership of presentation-neutral application state.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::event_projector::ApplicationEventProjector;
use super::session::PendingSwitch;
use super::session_state::SessionOrigin;
use crate::agent::events::{AgentCommand, RoutedEvent, StreamEvent};
use crate::agent::repeat::RepeatCommand;
use crate::api::types::ImageAttachment;
use crate::application::services::{DomainCommandPort, SettingsController};
use crate::application::session::ApplicationSession;
use crate::controlled_development::{
    ControlledDevelopmentCommand, ControlledDevelopmentEffect, ControlledDevelopmentTransitionError,
};
use crate::procedure::{
    ProcedureCommand, ProcedureProgress, ProcedureTerminalDisposition, RouteOverride,
};
use crate::search::SearchCommand;
use crate::voice::service::VoiceCommand;

use super::controlled_development_service::{
    ControlledDevelopmentEffectRequest, ControlledDevelopmentServiceEvent,
};
use super::dto::{
    AppChange, AppChangeKind, AppCommandRequest, AppCommandResult, AppError, AppErrorCode,
    AppRevision, AppSnapshot, ControlledDevelopmentView, OperationKind, OperationPhase,
    OperationState, PendingSessionSwitch, ProcedureRouteOverride, SessionSummary, TranscriptBlock,
    VisibleSettings, Workspace,
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
    pub(super) controlled_development:
        Option<DomainCommandPort<ControlledDevelopmentEffectRequest>>,
    pub(super) repeat_interrupt: Option<Arc<AtomicBool>>,
    pub(super) search_interrupt: Option<Arc<AtomicBool>>,
    pub(super) procedure_interrupt: Option<Arc<AtomicBool>>,
}

/// Process-private chat lifecycle dependencies used by every presentation adapter.
pub struct ChatLifecycle {
    pub(super) session: ApplicationSession,
    pub(super) agent: DomainCommandPort<AgentCommand>,
    pub(super) interrupt: Arc<AtomicBool>,
    pub(super) origin: SessionOrigin,
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
            controlled_development: None,
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
        self.snapshot.controlled_development = ControlledDevelopmentView::from_coordinator(
            chat.session.sessions.controlled_development(),
        );
        self.chat = Some(chat);
        self
    }

    pub fn connect_chat_lifecycle(&mut self, chat: ChatLifecycle) {
        self.snapshot.session = chat.session.session_summary();
        self.snapshot.saved_sessions = chat.session.saved_session_summaries();
        self.snapshot.controlled_development = ControlledDevelopmentView::from_coordinator(
            chat.session.sessions.controlled_development(),
        );
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

    pub fn connect_controlled_development(
        &mut self,
        controlled_development: DomainCommandPort<ControlledDevelopmentEffectRequest>,
    ) {
        self.controlled_development = Some(controlled_development);
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
        ApplicationEventProjector::new(self).apply_event(event)
    }

    /// Apply one selected-session completion from the slow controlled service.
    pub fn apply_controlled_development_event(
        &mut self,
        event: ControlledDevelopmentServiceEvent,
    ) -> Result<AppRevision, AppError> {
        if self.snapshot.session.id != event.session_id {
            return Err(invalid(
                "session_id",
                "controlled development event belongs to a session that is no longer selected",
            ));
        }
        self.apply_controlled_development_command(event.command)
    }

    pub(super) fn apply_controlled_development_command(
        &mut self,
        command: ControlledDevelopmentCommand,
    ) -> Result<AppRevision, AppError> {
        let persist_after_transition = !matches!(
            &command,
            ControlledDevelopmentCommand::RecordRawEvent { .. }
        );
        let effect = {
            let Some(chat) = &mut self.chat else {
                return Err(unavailable("chat lifecycle is not connected"));
            };
            chat.session
                .sessions
                .controlled_development_mut()
                .handle(command)
                .map_err(controlled_error)?
        };

        if let Some(effect) = effect
            && let Err(error) = self.dispatch_controlled_development_effect(effect)
        {
            let packet_id = self
                .chat
                .as_ref()
                .and_then(|chat| {
                    chat.session
                        .sessions
                        .controlled_development()
                        .state()
                        .packet_id()
                })
                .map(str::to_string);
            let Some(packet_id) = packet_id else {
                return Err(error);
            };
            let blocker = error.message;
            let chat = self.chat.as_mut().expect("controlled state was just read");
            chat.session
                .sessions
                .controlled_development_mut()
                .handle(ControlledDevelopmentCommand::Fail {
                    packet_id,
                    blocker: blocker.clone(),
                    summary: format!("Controlled Development blocked: {blocker}"),
                })
                .map_err(controlled_error)?;
        }
        if persist_after_transition {
            self.persist_controlled_development_state()?;
        }
        self.publish_controlled_development_state()
    }

    pub(super) fn publish_controlled_development_state(&mut self) -> Result<AppRevision, AppError> {
        let Some(chat) = &self.chat else {
            return Err(unavailable("chat lifecycle is not connected"));
        };
        let projected = ControlledDevelopmentView::from_coordinator(
            chat.session.sessions.controlled_development(),
        );
        self.snapshot.controlled_development = projected.clone();
        self.publish(AppChangeKind::ControlledDevelopmentChanged(projected))
    }

    pub(super) fn persist_controlled_development_state(&mut self) -> Result<(), AppError> {
        let Some(chat) = &mut self.chat else {
            return Err(unavailable("chat lifecycle is not connected"));
        };
        chat.session
            .sessions
            .autosave(&mut chat.session.transcript, chat.origin.clone());
        Ok(())
    }

    fn dispatch_controlled_development_effect(
        &mut self,
        effect: ControlledDevelopmentEffect,
    ) -> Result<(), AppError> {
        let Some(port) = &self.controlled_development else {
            return Err(unavailable(
                "controlled development service is not connected",
            ));
        };
        port.send(ControlledDevelopmentEffectRequest {
            session_id: self.snapshot.session.id.clone(),
            effect,
        })
        .map_err(|_| unavailable("controlled development service channel is closed"))
    }

    /// Project existing backend-neutral repeat and search events into browser state.
    pub fn apply_operation_stream_event(
        &mut self,
        event: &StreamEvent,
    ) -> Option<Result<AppRevision, AppError>> {
        ApplicationEventProjector::new(self).apply_operation_stream_event(event)
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
        ApplicationEventProjector::new(self).apply_routed_stream_event(routed)
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

    pub(super) fn operation_id(&self, kind: OperationKind) -> Option<String> {
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

    pub(super) fn apply_session_switch(&mut self, pending: PendingSwitch) -> AppCommandResult {
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
        self.snapshot.controlled_development = ControlledDevelopmentView::from_coordinator(
            chat.session.sessions.controlled_development(),
        );
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

    pub(super) fn finish_chat(
        &mut self,
        phase: OperationPhase,
        message: &str,
        append_terminal: bool,
    ) -> AppCommandResult {
        ApplicationEventProjector::new(self).finish_chat(phase, message, append_terminal)
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
            controlled_development: Default::default(),
            tests: Default::default(),
        }
    }
}

fn controlled_error(error: ControlledDevelopmentTransitionError) -> AppError {
    invalid("controlled_development", &error.to_string())
}
