use std::path::PathBuf;

use crate::agent::events::RoutedEvent;
use crate::procedure::{
    DisposableWorkspacePair, PromotionError, PromotionResult, VerifierGateEvidence, VerifierRun,
};

use super::ControlledBackendSelection;

/// Typed input accepted by one session-owned Controlled Development service.
#[derive(Debug)]
pub enum ControlledDevelopmentCommand {
    SetEnabled {
        enabled: bool,
    },
    Plan {
        packet_id: String,
        original_request: String,
        selection: ControlledBackendSelection,
        workspace_root: PathBuf,
    },
    PlanningFinished {
        packet_id: String,
        final_response: String,
    },
    Approve {
        card_id: String,
    },
    Reject {
        card_id: String,
    },
    Stop {
        packet_id: String,
    },
    DiscardRetainedEvidence,
    ExecutionWorkspaceReady {
        card_id: String,
        workspace: Box<DisposableWorkspacePair>,
    },
    RecordRawEvent {
        packet_id: String,
        event: RoutedEvent,
    },
    RecordProofEvidence {
        card_id: String,
        evidence: Box<VerifierGateEvidence>,
    },
    ValidateIsolatedChanges {
        card_id: String,
    },
    ProofsFinished {
        card_id: String,
        run: Box<VerifierRun>,
    },
    PromotionFinished {
        card_id: String,
        result: Result<Box<PromotionResult>, Box<PromotionError>>,
    },
    Complete {
        card_id: String,
        summary: String,
    },
    Fail {
        packet_id: String,
        blocker: String,
        summary: String,
    },
    Interrupt {
        packet_id: String,
        summary: String,
    },
}
