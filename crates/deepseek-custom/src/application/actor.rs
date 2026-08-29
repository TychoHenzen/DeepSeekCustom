//! Serialized ownership of presentation-neutral application state.

use std::collections::VecDeque;

use super::dto::{
    AppChange, AppChangeKind, AppCommand, AppCommandRequest, AppCommandResult, AppError,
    AppErrorCode, AppRevision, AppSnapshot, PendingSessionSwitch, SessionSummary, TranscriptBlock,
    VisibleSettings, Workspace,
};

/// Result of asking the actor for changes after a known revision.
#[derive(Debug, Clone, PartialEq)]
pub enum Replay {
    /// Every change after the requested revision is still retained.
    Changes(Vec<AppChange>),
    /// The requested revision predates retained history. Replace local state.
    Reset(AppSnapshot),
}

/// A domain or service event already projected into presentation-neutral data.
#[derive(Debug, Clone, PartialEq)]
pub enum AppEvent {
    TranscriptAppended(TranscriptBlock),
    SessionChanged(SessionSummary),
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
}

impl ApplicationActor {
    pub fn new(mut snapshot: AppSnapshot, replay_capacity: usize) -> Self {
        snapshot.revision = AppRevision::INITIAL;
        Self {
            snapshot,
            replay_capacity,
            changes: VecDeque::with_capacity(replay_capacity),
        }
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
            _ => AppCommandResult::Rejected {
                error: unavailable("command is not connected to a domain port yet"),
            },
        }
    }

    pub fn apply_event(&mut self, event: AppEvent) -> Result<AppRevision, AppError> {
        let change = match event {
            AppEvent::TranscriptAppended(block) => {
                self.snapshot.transcript.push(block.clone());
                AppChangeKind::TranscriptAppended(block)
            }
            AppEvent::SessionChanged(session) => {
                self.snapshot.session = session.clone();
                AppChangeKind::SessionChanged(session)
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
            return Replay::Reset(self.snapshot.clone());
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
            return Replay::Reset(self.snapshot.clone());
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
}

fn unavailable(message: &str) -> AppError {
    AppError {
        code: AppErrorCode::Unavailable,
        message: message.to_string(),
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
            pending_session_switch: None,
            settings,
            operations: Vec::new(),
        }
    }
}
