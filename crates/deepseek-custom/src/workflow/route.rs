use super::{WorkflowStep, WorkflowStepOutcome};

/// The one guarded route shared by every workflow run.
#[derive(Debug, Clone, Copy, Default)]
pub struct WorkflowRoute;

impl WorkflowRoute {
    pub const STEPS: [WorkflowStep; 7] = [
        WorkflowStep::Capture,
        WorkflowStep::Refine,
        WorkflowStep::Implement,
        WorkflowStep::DraftPullRequest,
        WorkflowStep::Review,
        WorkflowStep::FixFindings,
        WorkflowStep::CompletionGate,
    ];

    pub const fn steps(&self) -> &[WorkflowStep; 7] {
        &Self::STEPS
    }

    pub fn validate(&self, step: WorkflowStep, outcome: &WorkflowStepOutcome) -> bool {
        match outcome {
            WorkflowStepOutcome::Completed { .. } => {
                step.next().is_some() || step == WorkflowStep::CompletionGate
            }
            WorkflowStepOutcome::AwaitingApproval { .. } => step.is_human_gate(),
            WorkflowStepOutcome::Retryable { .. }
            | WorkflowStepOutcome::Blocked { .. }
            | WorkflowStepOutcome::NeedsDecision { .. }
            | WorkflowStepOutcome::Failed { .. } => true,
        }
    }
}
