//! Projection of domain and routed agent events into application changes.

use std::collections::VecDeque;

use crate::agent::events::{RoutedEvent, StreamEvent};

use super::actor::{AppEvent, ApplicationActor, unavailable};
use super::dto::{
    AppChangeKind, AppCommandResult, AppError, AppRevision, OperationKind, OperationPhase,
    OperationState, TranscriptBlock, TranscriptContent,
};
use super::operation_projection::project_operation;
use super::transcript::{BlockKind, Severity};

/// Converts ordered domain events into presentation-neutral application state.
pub struct ApplicationEventProjector<'a> {
    actor: &'a mut ApplicationActor,
}

impl<'a> ApplicationEventProjector<'a> {
    pub fn new(actor: &'a mut ApplicationActor) -> Self {
        Self { actor }
    }

    pub fn apply_event(&mut self, event: AppEvent) -> Result<AppRevision, AppError> {
        if let AppEvent::TranscriptAppended(TranscriptBlock {
            content: TranscriptContent::Terminal { outcome, message },
            ..
        }) = &event
            && self
                .actor
                .chat
                .as_ref()
                .is_some_and(|chat| chat.session.turn_active)
        {
            return command_result(self.finish_chat(*outcome, message, true));
        }
        let change = match event {
            AppEvent::TranscriptAppended(block) => {
                self.actor.snapshot.transcript.push(block.clone());
                AppChangeKind::TranscriptAppended(block)
            }
            AppEvent::SessionChanged(session) => {
                self.actor.snapshot.session = session.clone();
                AppChangeKind::SessionChanged(session)
            }
            AppEvent::SavedSessionsChanged(sessions) => {
                self.actor.snapshot.saved_sessions = sessions.clone();
                AppChangeKind::SavedSessionsChanged(sessions)
            }
            AppEvent::PendingSessionSwitchChanged(pending) => {
                self.actor.snapshot.pending_session_switch = pending.clone();
                AppChangeKind::PendingSessionSwitchChanged(pending)
            }
            AppEvent::SettingsChanged(settings) => {
                self.actor.snapshot.settings = settings.clone();
                AppChangeKind::SettingsChanged(settings)
            }
            AppEvent::OperationChanged(operation) => {
                if let Some(existing) = self
                    .actor
                    .snapshot
                    .operations
                    .iter_mut()
                    .find(|existing| existing.kind == operation.kind)
                {
                    *existing = operation.clone();
                } else {
                    self.actor.snapshot.operations.push(operation.clone());
                }
                AppChangeKind::OperationChanged(operation)
            }
            AppEvent::TestsChanged(tests) => {
                self.actor.snapshot.tests = tests.as_ref().clone();
                AppChangeKind::TestsChanged(tests)
            }
            AppEvent::Error(error) => AppChangeKind::Error(error),
        };
        self.actor.publish(change)
    }

    pub fn apply_operation_stream_event(
        &mut self,
        event: &StreamEvent,
    ) -> Option<Result<AppRevision, AppError>> {
        let state = project_operation(self.actor, event)?;
        Some(self.apply_event(AppEvent::OperationChanged(state)))
    }

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
                    if let Some(chat) = &mut self.actor.chat {
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
        self.actor.chat.as_ref()?;
        let terminal_event = routed.event.clone();
        if let Some(chat) = &mut self.actor.chat {
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
                    .actor
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
                    .actor
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
            _ => Some(Ok(self.actor.snapshot.revision)),
        }
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
                    .actor
                    .snapshot
                    .transcript
                    .iter()
                    .fold(0, |highest, block| highest.max(block.id))
                    + 1,
                content: TranscriptContent::Terminal {
                    outcome: phase,
                    message: message.into(),
                },
            };
            self.actor.snapshot.transcript.push(terminal.clone());
            if self
                .actor
                .publish(AppChangeKind::TranscriptAppended(terminal))
                .is_err()
            {
                return AppCommandResult::Rejected {
                    error: unavailable("could not publish terminal event"),
                };
            }
        }
        let pending = {
            let chat = self.actor.chat.as_mut().expect("checked by caller");
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
                .actor
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
        self.actor
            .snapshot
            .operations
            .retain(|item| item.kind != OperationKind::Chat);
        self.actor.snapshot.operations.push(operation.clone());
        let revision = match self
            .actor
            .publish(AppChangeKind::OperationChanged(operation))
        {
            Ok(revision) => revision,
            Err(error) => return AppCommandResult::Rejected { error },
        };
        if let Some(pending) = pending {
            return self.actor.apply_session_switch(pending);
        }
        AppCommandResult::Applied { revision }
    }

    fn publish_chat_transcript_reset(&mut self) -> Result<AppRevision, AppError> {
        let mut image_flags = self
            .actor
            .snapshot
            .transcript
            .iter()
            .filter_map(|block| match &block.content {
                TranscriptContent::User { has_image, .. } => Some(*has_image),
                _ => None,
            })
            .collect::<VecDeque<_>>();
        let mut transcript = self
            .actor
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
        self.actor.snapshot.transcript = transcript;
        self.publish_reset()
    }

    fn apply_agent_session_reset(&mut self) -> Result<AppRevision, AppError> {
        let Some(chat) = &mut self.actor.chat else {
            return Err(unavailable("chat lifecycle is not connected"));
        };
        chat.session
            .sessions
            .save_outgoing_and_start_new(&mut chat.session.transcript, chat.origin.clone());
        self.actor.snapshot.transcript = chat.session.transcript_projection();
        self.actor.snapshot.session = chat.session.session_summary();
        self.actor.snapshot.saved_sessions = chat.session.saved_session_summaries();
        self.actor.snapshot.pending_session_switch = None;
        self.publish_reset()
    }

    fn publish_reset(&mut self) -> Result<AppRevision, AppError> {
        let mut reset = self.actor.snapshot.clone();
        reset.revision = self
            .actor
            .snapshot
            .revision
            .checked_next()
            .ok_or_else(|| unavailable("application revision exhausted"))?;
        self.actor.publish(AppChangeKind::Reset(Box::new(reset)))
    }
}

fn command_result(result: AppCommandResult) -> Result<AppRevision, AppError> {
    match result {
        AppCommandResult::Applied { revision } => Ok(revision),
        AppCommandResult::Rejected { error } => Err(error),
        AppCommandResult::Conflict { .. } => unreachable!("internal events cannot conflict"),
    }
}
