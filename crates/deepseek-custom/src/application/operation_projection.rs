use std::sync::atomic::Ordering;

use crate::agent::events::StreamEvent;
use crate::search::{SearchKind, SearchSnapshot};

use super::actor::ApplicationActor;
use super::dto::{
    AppError, AppErrorCode, OperationKind, OperationPhase, OperationProgress, OperationState,
};

pub(super) fn project_operation(
    actor: &ApplicationActor,
    event: &StreamEvent,
) -> Option<OperationState> {
    match event {
        StreamEvent::RepeatIterationStart { index, total, .. } => Some(OperationState {
            kind: OperationKind::Autopilot,
            operation_id: actor.operation_id(OperationKind::Autopilot),
            phase: OperationPhase::Running,
            progress: Some(OperationProgress {
                completed: u64::from(index.saturating_sub(1)),
                total: Some(u64::from(*total)),
            }),
            message: Some(format!("Running iteration {index} of {total}")),
            error: None,
        }),
        StreamEvent::RepeatFinished { completed, total } => Some(OperationState {
            kind: OperationKind::Autopilot,
            operation_id: actor.operation_id(OperationKind::Autopilot),
            phase: if *completed < *total
                && actor
                    .repeat_interrupt
                    .as_ref()
                    .is_some_and(|flag| flag.load(Ordering::SeqCst))
            {
                OperationPhase::Interrupted
            } else {
                OperationPhase::Completed
            },
            progress: Some(OperationProgress {
                completed: u64::from(*completed),
                total: Some(u64::from(*total)),
            }),
            message: Some(format!(
                "Autopilot finished: {completed} of {total} iterations"
            )),
            error: None,
        }),
        StreamEvent::SearchProgress(snapshot) => Some(OperationState {
            kind: operation_kind(snapshot.kind),
            operation_id: actor.operation_id(operation_kind(snapshot.kind)),
            phase: OperationPhase::Running,
            progress: Some(OperationProgress {
                completed: u64::from(snapshot.done),
                total: Some(u64::from(snapshot.total)),
            }),
            message: Some(search_message(snapshot)),
            error: None,
        }),
        StreamEvent::SearchFinished {
            kind,
            summary,
            is_error,
        } => Some(OperationState {
            kind: operation_kind(*kind),
            operation_id: actor.operation_id(operation_kind(*kind)),
            phase: if actor
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
            progress: actor
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
        }),
        _ => None,
    }
}

fn operation_kind(kind: SearchKind) -> OperationKind {
    match kind {
        SearchKind::Cascade => OperationKind::Cascade,
        SearchKind::Evolve => OperationKind::Evolve,
    }
}

fn search_message(snapshot: &SearchSnapshot) -> String {
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
