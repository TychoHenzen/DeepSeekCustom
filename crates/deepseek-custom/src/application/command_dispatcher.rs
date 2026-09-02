//! Command validation and typed domain-port dispatch.

use std::sync::atomic::Ordering;

use super::actor::{
    ApplicationActor, invalid, operation_active, snapshot_effort, trimmed_lines, trimmed_option,
    unavailable,
};
use super::dto::{
    AppChangeKind, AppCommand, AppCommandRequest, AppCommandResult, AppError, AppErrorCode,
    OperationKind, OperationPhase, OperationState, ReviewDecision, TranscriptBlock,
    TranscriptContent,
};
use super::session::PendingSwitch;
use super::transcript::{BlockKind, Span};
use crate::agent::events::AgentCommand;
use crate::agent::repeat::RepeatCommand;
use crate::controlled_development::ControlledDevelopmentControlInput;
use crate::procedure::{
    ApplyRequest, PatchPreviewId, PatchPreviewRequest, ProcedureCommand, ProcedureReviewDecision,
    ProcedureRunId, ProcedureRunRequest, ProcedureScratchpad, WholeChangeCommandRequest,
};
use crate::search::SearchCommand;
use crate::search::cascade::{CascadeParams, MAX_ATTEMPTS};
use crate::search::evolve::{EvolveParams, MAX_TOTAL_DISPATCHES};
use crate::session::SessionId;
use crate::voice::service::VoiceCommand;

/// Validates application commands and dispatches them through typed domain ports.
pub struct ApplicationCommandDispatcher<'a> {
    actor: &'a mut ApplicationActor,
}

impl<'a> ApplicationCommandDispatcher<'a> {
    pub fn new(actor: &'a mut ApplicationActor) -> Self {
        Self { actor }
    }

    pub fn dispatch(&mut self, request: AppCommandRequest) -> AppCommandResult {
        if request.revision != self.actor.snapshot.revision {
            return AppCommandResult::Conflict {
                current_revision: self.actor.snapshot.revision,
            };
        }
        match request.command {
            AppCommand::SelectWorkspace { workspace } => {
                self.actor.snapshot.workspace = workspace;
                match self
                    .actor
                    .publish(AppChangeKind::WorkspaceSelected(workspace))
                {
                    Ok(revision) => AppCommandResult::Applied { revision },
                    Err(error) => AppCommandResult::Rejected { error },
                }
            }
            AppCommand::SendMessage {
                text,
                attachment_id,
            } => {
                if attachment_id.is_none()
                    && self.actor.chat.as_ref().is_some_and(|chat| {
                        chat.session
                            .sessions
                            .controlled_development()
                            .state()
                            .is_enabled()
                    })
                    && let Some(input) = ControlledDevelopmentControlInput::parse(&text)
                {
                    return self.dispatch_control_input(text, input);
                }
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
                    Some(id) => match self.actor.attachments.get(id) {
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
                if let Some(chat) = &mut self.actor.chat {
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
                    self.actor.attachments.remove(id);
                }
                let id = self
                    .actor
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
                self.actor.snapshot.transcript.push(user.clone());
                if let Err(error) = self.actor.publish(AppChangeKind::TranscriptAppended(user)) {
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
                self.actor
                    .snapshot
                    .operations
                    .retain(|item| item.kind != OperationKind::Chat);
                self.actor.snapshot.operations.push(operation.clone());
                match self
                    .actor
                    .publish(AppChangeKind::OperationChanged(operation))
                {
                    Ok(revision) => AppCommandResult::Applied { revision },
                    Err(error) => AppCommandResult::Rejected { error },
                }
            }
            AppCommand::StopOperation {
                kind: OperationKind::Chat,
            } => {
                let Some(chat) = &mut self.actor.chat else {
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
                self.actor
                    .finish_chat(OperationPhase::Interrupted, "Interrupted by user", true)
            }
            AppCommand::StopOperation {
                kind: OperationKind::Autopilot,
            } => self
                .actor
                .stop_flagged_operation(OperationKind::Autopilot, false),
            AppCommand::StopOperation {
                kind: kind @ (OperationKind::Cascade | OperationKind::Evolve),
            } => self.actor.stop_flagged_operation(kind, true),
            AppCommand::StopOperation {
                kind: OperationKind::Procedure,
            } => self.actor.stop_procedure(),
            AppCommand::NewSession => self.actor.request_session_switch(PendingSwitch::New),
            AppCommand::LoadSession { session_id } => {
                let Ok(id) = SessionId::parse(&session_id) else {
                    return AppCommandResult::Rejected {
                        error: invalid("session_id", "session id is invalid"),
                    };
                };
                self.actor.request_session_switch(PendingSwitch::Load(id))
            }
            AppCommand::DeleteSession { session_id } => {
                let Ok(id) = SessionId::parse(&session_id) else {
                    return AppCommandResult::Rejected {
                        error: invalid("session_id", "session id is invalid"),
                    };
                };
                let Some(chat) = &mut self.actor.chat else {
                    return AppCommandResult::Rejected {
                        error: unavailable("chat lifecycle is not connected"),
                    };
                };
                chat.session.sessions.delete(id);
                self.actor.snapshot.saved_sessions = chat.session.saved_session_summaries();
                match self.actor.publish(AppChangeKind::SavedSessionsChanged(
                    self.actor.snapshot.saved_sessions.clone(),
                )) {
                    Ok(revision) => AppCommandResult::Applied { revision },
                    Err(error) => AppCommandResult::Rejected { error },
                }
            }
            AppCommand::UpdateSettings { settings } => {
                let Some(controller) = &self.actor.settings else {
                    return AppCommandResult::Rejected {
                        error: unavailable("settings lifecycle is not connected"),
                    };
                };
                match controller.update(*settings) {
                    Ok(settings) => {
                        if let Some(voice) = &self.actor.voice {
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
                        self.actor.snapshot.settings = settings.clone();
                        match self.actor.publish(AppChangeKind::SettingsChanged(settings)) {
                            Ok(revision) => AppCommandResult::Applied { revision },
                            Err(error) => AppCommandResult::Rejected { error },
                        }
                    }
                    Err(error) => AppCommandResult::Rejected { error },
                }
            }
            AppCommand::StartVoiceCapture => {
                let Some(voice) = &self.actor.voice else {
                    return AppCommandResult::Rejected {
                        error: unavailable("voice service is not connected"),
                    };
                };
                if voice.send(VoiceCommand::StartListening).is_err() {
                    return AppCommandResult::Rejected {
                        error: unavailable("voice command channel is closed"),
                    };
                }
                self.actor.publish_voice_operation("Listening")
            }
            AppCommand::StopVoiceCapture => {
                let Some(voice) = &self.actor.voice else {
                    return AppCommandResult::Rejected {
                        error: unavailable("voice service is not connected"),
                    };
                };
                if voice.send(VoiceCommand::StopListening).is_err() {
                    return AppCommandResult::Rejected {
                        error: unavailable("voice command channel is closed"),
                    };
                }
                self.actor.publish_voice_operation("Transcribing")
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
                let Some(port) = &self.actor.autopilot else {
                    return AppCommandResult::Rejected {
                        error: unavailable("autopilot service is not connected"),
                    };
                };
                if self.actor.has_active_operation() {
                    return AppCommandResult::Rejected {
                        error: operation_active("another operation is already running"),
                    };
                }
                if let Some(flag) = &self.actor.repeat_interrupt {
                    flag.store(false, Ordering::SeqCst);
                }
                if port.send(RepeatCommand { task, iterations }).is_err() {
                    return AppCommandResult::Rejected {
                        error: unavailable("autopilot command channel is closed"),
                    };
                }
                self.actor.start_operation(
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
                    effort: snapshot_effort(&self.actor.snapshot.settings.effort),
                };
                self.actor.start_search(
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
                    effort: snapshot_effort(&self.actor.snapshot.settings.effort),
                };
                self.actor.start_search(
                    SearchCommand::Evolve(Box::new(params)),
                    OperationKind::Evolve,
                    Some(u64::from(planned.min(MAX_TOTAL_DISPATCHES))),
                )
            }
            AppCommand::RunProcedure { change_id, task_id } => {
                let change_id = change_id.trim().to_string();
                let task_id = task_id.trim().to_string();
                if change_id.is_empty() {
                    return AppCommandResult::Rejected {
                        error: invalid("change_id", "change id is required"),
                    };
                }
                if task_id.is_empty() {
                    return AppCommandResult::Rejected {
                        error: invalid("task_id", "task id is required"),
                    };
                }
                if self.actor.has_active_operation() {
                    return AppCommandResult::Rejected {
                        error: operation_active("another operation is already running"),
                    };
                }
                let Some(port) = &self.actor.procedure else {
                    return AppCommandResult::Rejected {
                        error: unavailable("procedure service is not connected"),
                    };
                };
                let Some(backend) = self
                    .actor
                    .snapshot
                    .settings
                    .procedure
                    .localization_backend
                    .clone()
                else {
                    return AppCommandResult::Rejected {
                        error: invalid(
                            "localization_backend",
                            "procedure localization backend is required",
                        ),
                    };
                };
                let run_id = ProcedureRunId::new();
                if let Some(flag) = &self.actor.procedure_interrupt {
                    flag.store(false, Ordering::SeqCst);
                }
                if port
                    .send(ProcedureCommand::Run {
                        run_id,
                        backend,
                        request: ProcedureRunRequest {
                            change_id,
                            task_id,
                            scratchpad: ProcedureScratchpad::default(),
                        },
                    })
                    .is_err()
                {
                    return AppCommandResult::Rejected {
                        error: unavailable("procedure command channel is closed"),
                    };
                }
                self.actor.start_operation_with_id(
                    OperationKind::Procedure,
                    Some(run_id.as_str()),
                    None,
                    "Procedure started",
                )
            }
            AppCommand::PreviewProcedure {
                localization_run_id,
                change_id,
                task_id,
                route,
                local_backend,
                local_model,
                frontier_backend,
                frontier_model,
            } => {
                let Some(port) = &self.actor.procedure else {
                    return AppCommandResult::Rejected {
                        error: unavailable("procedure service is not connected"),
                    };
                };
                let Ok(localization_run_id) = ProcedureRunId::parse(&localization_run_id) else {
                    return AppCommandResult::Rejected {
                        error: invalid("localization_run_id", "localization run id is invalid"),
                    };
                };
                if [
                    change_id.as_str(),
                    task_id.as_str(),
                    local_backend.as_str(),
                    local_model.as_str(),
                    frontier_backend.as_str(),
                    frontier_model.as_str(),
                ]
                .iter()
                .any(|value| value.trim().is_empty())
                {
                    return AppCommandResult::Rejected {
                        error: invalid("procedure_preview", "all preview fields are required"),
                    };
                }
                if self.actor.has_active_operation() {
                    return AppCommandResult::Rejected {
                        error: operation_active("another operation is already running"),
                    };
                }
                let preview_id = PatchPreviewId::new();
                let request = PatchPreviewRequest {
                    localization_run_id,
                    change_id,
                    task_id,
                    route_override: route.into(),
                    local_backend,
                    local_model,
                    frontier_backend,
                    frontier_model,
                };
                if port
                    .send(ProcedureCommand::Preview {
                        preview_id,
                        request,
                    })
                    .is_err()
                {
                    return AppCommandResult::Rejected {
                        error: unavailable("procedure command channel is closed"),
                    };
                }
                self.actor
                    .start_procedure_command(preview_id.as_str(), "Procedure preview started")
            }
            AppCommand::RunWholeChangeProcedure {
                change_id,
                route,
                localization_backend,
                local_backend,
                local_model,
                frontier_backend,
                frontier_model,
            } => {
                let Some(port) = &self.actor.procedure else {
                    return AppCommandResult::Rejected {
                        error: unavailable("procedure service is not connected"),
                    };
                };
                if [
                    change_id.as_str(),
                    localization_backend.as_str(),
                    local_backend.as_str(),
                    local_model.as_str(),
                    frontier_backend.as_str(),
                    frontier_model.as_str(),
                ]
                .iter()
                .any(|value| value.trim().is_empty())
                {
                    return AppCommandResult::Rejected {
                        error: invalid("whole_change", "all whole-change fields are required"),
                    };
                }
                if self.actor.has_active_operation() {
                    return AppCommandResult::Rejected {
                        error: operation_active("another operation is already running"),
                    };
                }
                let run_id = ProcedureRunId::new();
                let request = WholeChangeCommandRequest {
                    change_id,
                    route_override: route.into(),
                    localization_backend,
                    local_backend,
                    local_model,
                    frontier_backend,
                    frontier_model,
                };
                if port
                    .send(ProcedureCommand::WholeChange { run_id, request })
                    .is_err()
                {
                    return AppCommandResult::Rejected {
                        error: unavailable("procedure command channel is closed"),
                    };
                }
                self.actor
                    .start_procedure_command(run_id.as_str(), "Whole-change Procedure started")
            }
            AppCommand::ApplyProcedure {
                localization_run_id,
                preview_id,
                change_id,
                task_id,
            } => {
                let Some(port) = &self.actor.procedure else {
                    return AppCommandResult::Rejected {
                        error: unavailable("procedure service is not connected"),
                    };
                };
                let Ok(localization_run_id) = ProcedureRunId::parse(&localization_run_id) else {
                    return AppCommandResult::Rejected {
                        error: invalid("localization_run_id", "localization run id is invalid"),
                    };
                };
                let Ok(preview_id) = PatchPreviewId::parse(&preview_id) else {
                    return AppCommandResult::Rejected {
                        error: invalid("preview_id", "preview id is invalid"),
                    };
                };
                if change_id.trim().is_empty() || task_id.trim().is_empty() {
                    return AppCommandResult::Rejected {
                        error: invalid("procedure_apply", "change id and task id are required"),
                    };
                }
                if self.actor.has_active_operation() {
                    return AppCommandResult::Rejected {
                        error: operation_active("another operation is already running"),
                    };
                }
                let run_id = ProcedureRunId::new();
                let request = ApplyRequest {
                    localization_run_id,
                    preview_id,
                    change_id,
                    task_id,
                };
                if port
                    .send(ProcedureCommand::Apply { run_id, request })
                    .is_err()
                {
                    return AppCommandResult::Rejected {
                        error: unavailable("procedure command channel is closed"),
                    };
                }
                self.actor
                    .start_procedure_command(run_id.as_str(), "Procedure apply started")
            }
            AppCommand::ReviewProcedure { run_id, decision } => {
                let current_run = self.actor.snapshot.operations.iter().find(|operation| {
                    operation.kind == OperationKind::Procedure
                        && operation.phase == OperationPhase::AwaitingReview
                });
                if current_run.and_then(|operation| operation.operation_id.as_deref())
                    != Some(run_id.as_str())
                {
                    return AppCommandResult::Rejected {
                        error: invalid("run_id", "procedure review run is no longer current"),
                    };
                }
                let Ok(run_id) = ProcedureRunId::parse(&run_id) else {
                    return AppCommandResult::Rejected {
                        error: invalid("run_id", "procedure run id is invalid"),
                    };
                };
                let Some(port) = &self.actor.procedure else {
                    return AppCommandResult::Rejected {
                        error: unavailable("procedure service is not connected"),
                    };
                };
                let decision = match decision {
                    ReviewDecision::Approve => ProcedureReviewDecision::Approve,
                    ReviewDecision::Reject => ProcedureReviewDecision::Reject,
                };
                if port
                    .send(ProcedureCommand::Review { run_id, decision })
                    .is_err()
                {
                    return AppCommandResult::Rejected {
                        error: unavailable("procedure command channel is closed"),
                    };
                }
                match self.actor.publish(AppChangeKind::OperationChanged(
                    current_run.unwrap().clone(),
                )) {
                    Ok(revision) => AppCommandResult::Applied { revision },
                    Err(error) => AppCommandResult::Rejected { error },
                }
            }
            _ => AppCommandResult::Rejected {
                error: unavailable("command is not connected to a domain port yet"),
            },
        }
    }

    fn dispatch_control_input(
        &mut self,
        text: String,
        input: ControlledDevelopmentControlInput,
    ) -> AppCommandResult {
        let response = {
            let Some(chat) = &mut self.actor.chat else {
                return AppCommandResult::Rejected {
                    error: unavailable("chat lifecycle is not connected"),
                };
            };
            match chat
                .session
                .sessions
                .controlled_development_mut()
                .handle_control_input(input)
            {
                Ok(response) => response,
                Err(error) => {
                    return AppCommandResult::Rejected {
                        error: invalid("message", &error.to_string()),
                    };
                }
            }
        };

        let blocks = {
            let chat = self
                .actor
                .chat
                .as_mut()
                .expect("controlled input requires a connected chat lifecycle");
            chat.session
                .transcript
                .push(BlockKind::User { text: text.clone() });
            chat.session.transcript.push(BlockKind::Assistant {
                spans: vec![Span::Text(response)],
            });
            let projection = chat.session.transcript_projection();
            projection[projection.len() - 2..].to_vec()
        };

        let mut revision = self.actor.snapshot.revision;
        for block in blocks {
            self.actor.snapshot.transcript.push(block.clone());
            revision = match self.actor.publish(AppChangeKind::TranscriptAppended(block)) {
                Ok(revision) => revision,
                Err(error) => return AppCommandResult::Rejected { error },
            };
        }
        AppCommandResult::Applied { revision }
    }
}
