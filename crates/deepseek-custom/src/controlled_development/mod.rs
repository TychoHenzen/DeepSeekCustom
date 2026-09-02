//! Session-scoped contracts for Controlled Development.

mod authorized_workspace_changes;
mod backend_selection;
mod change_gate;
mod change_gate_error;
mod coordinator;
mod dependency_change_inspector;
mod dependency_exception_match;
mod dependency_file_classifier;
mod dependency_file_kind;
mod diagnostic_diff;
mod phase;
mod promotion_plan;
mod retained_workspace;
mod service_command;
mod service_effect;
mod state;
mod transition_error;
mod work_card;
mod work_card_schema;
mod work_card_validation_error;
mod work_card_validation_errors;

pub use authorized_workspace_changes::AuthorizedWorkspaceChanges;
pub use backend_selection::ControlledBackendSelection;
pub use change_gate::{authorize_changed_paths, authorize_workspace_changes};
pub use change_gate_error::ControlledChangeGateError;
pub use coordinator::ControlledDevelopmentCoordinator;
pub use dependency_exception_match::DependencyExceptionMatch;
pub use dependency_file_classifier::classify_dependency_file;
pub use dependency_file_kind::DependencyFileKind;
pub use phase::ControlledDevelopmentPhase;
pub use promotion_plan::PROJECT_STATE_PATH;
pub use retained_workspace::ControlledDevelopmentRetainedWorkspace;
pub use service_command::ControlledDevelopmentCommand;
pub use service_effect::ControlledDevelopmentEffect;
pub use state::ControlledDevelopmentState;
pub use transition_error::ControlledDevelopmentTransitionError;
pub use work_card::{
    MAX_COMPLEXITY_EXCEPTIONS, MAX_EXCLUSIONS, MAX_PRODUCTION_PATHS, MAX_PROOF_COMMAND_CHARS,
    MAX_PROOF_COMMANDS, MAX_SUPPORTING_PATHS, MAX_WORK_CARD_ID_CHARS, MAX_WORK_CARD_ITEM_CHARS,
    MAX_WORK_CARD_OUTCOME_CHARS, MAX_WORK_CARD_PATH_CHARS, WorkCard,
};
pub use work_card_schema::work_card_json_schema;
pub use work_card_validation_error::WorkCardValidationError;
pub use work_card_validation_errors::WorkCardValidationErrors;
