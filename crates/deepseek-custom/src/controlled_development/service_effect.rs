use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::mpsc;

use crate::agent::events::RoutedEvent;
use crate::backend::Backend;
use crate::backend::factory::BackendFactory;
use crate::procedure::{
    PromotionBaseline, PromotionError, PromotionResult, PromotionTarget, VerifierCommandRunner,
    VerifierRun, promote_verified_workspace,
};

use super::{ControlledBackendSelection, WorkCard};

/// Slow work requested by one accepted coordinator transition.
///
/// Planning and execution effects carry the captured backend selection and
/// fixed root needed to build a fresh controlled backend. Workspace creation
/// remains a separate effect so rejection cannot accidentally dispatch it.
#[derive(Debug)]
pub enum ControlledDevelopmentEffect {
    DispatchPlanning {
        packet_id: String,
        original_request: String,
        selection: ControlledBackendSelection,
        planning_root: PathBuf,
    },
    CreateExecutionWorkspace {
        card_id: String,
        project_root: PathBuf,
    },
    DispatchExecution {
        card_id: String,
        original_request: String,
        card: WorkCard,
        selection: ControlledBackendSelection,
        execution_root: PathBuf,
    },
    RunProofCommands {
        card_id: String,
        proof_commands: Vec<String>,
        execution_root: PathBuf,
        interrupt: Arc<std::sync::atomic::AtomicBool>,
    },
    PromoteValidatedChanges {
        card_id: String,
        project_root: PathBuf,
        execution_root: PathBuf,
        baseline: PromotionBaseline,
        targets: Vec<PromotionTarget>,
    },
}

impl ControlledDevelopmentEffect {
    /// Build the fresh backend required by a dispatch effect.
    ///
    /// Workspace creation has no backend and returns `None`. The factory's
    /// normal working directory and session flags are never changed.
    pub fn build_fresh_backend(
        self,
        factory: &Arc<BackendFactory>,
        events: mpsc::UnboundedSender<RoutedEvent>,
    ) -> Result<Option<Backend>, String> {
        match self {
            Self::DispatchPlanning {
                selection,
                planning_root,
                ..
            } => factory
                .build_controlled_planning(
                    &selection.backend,
                    selection.model.as_deref(),
                    events,
                    planning_root,
                )
                .map(Some),
            Self::DispatchExecution {
                selection,
                execution_root,
                ..
            } => factory
                .build_controlled_execution(
                    &selection.backend,
                    selection.model.as_deref(),
                    events,
                    execution_root,
                )
                .map(Some),
            Self::CreateExecutionWorkspace { .. } => Ok(None),
            Self::RunProofCommands { .. } | Self::PromoteValidatedChanges { .. } => Ok(None),
        }
    }

    /// Execute an ordered proof effect with the packet's shared interrupt flag.
    pub async fn run_proof_commands(&self) -> Option<VerifierRun> {
        let Self::RunProofCommands {
            proof_commands,
            execution_root,
            interrupt,
            ..
        } = self
        else {
            return None;
        };
        Some(
            VerifierCommandRunner::with_interrupt(Arc::clone(interrupt))
                .run(execution_root, proof_commands)
                .await,
        )
    }

    /// Execute the recoverable Procedure transaction carried by a promotion effect.
    pub fn promote_validated_changes(&self) -> Option<Result<PromotionResult, PromotionError>> {
        let Self::PromoteValidatedChanges {
            project_root,
            execution_root,
            baseline,
            targets,
            ..
        } = self
        else {
            return None;
        };
        Some(promote_verified_workspace(
            project_root,
            execution_root,
            baseline,
            targets,
        ))
    }
}
