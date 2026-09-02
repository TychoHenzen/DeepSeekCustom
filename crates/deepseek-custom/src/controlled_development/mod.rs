//! Session-scoped contracts for Controlled Development.

mod authorized_workspace_changes;
mod backend_selection;
mod change_gate;
mod change_gate_error;
mod compact_evidence;
mod control_input;
mod coordinator;
mod dependency_change_inspector;
mod dependency_exception_match;
mod dependency_file_classifier;
mod dependency_file_kind;
mod diagnostic_diff;
mod owned_workspace_root;
mod phase;
mod project_state_builder;
mod project_state_input;
mod promotion_plan;
mod retained_workspace;
mod retained_workspace_reference;
mod service_command;
mod service_effect;
mod session_record;
mod state;
mod system_map_component;
mod transition_error;
mod work_card;
mod work_card_schema;
mod work_card_validation_error;
mod work_card_validation_errors;

pub use authorized_workspace_changes::AuthorizedWorkspaceChanges;
pub use backend_selection::ControlledBackendSelection;
pub use change_gate::{authorize_changed_paths, authorize_workspace_changes};
pub use change_gate_error::ControlledChangeGateError;
pub use compact_evidence::ControlledDevelopmentCompactEvidence;
pub use control_input::ControlledDevelopmentControlInput;
pub use coordinator::ControlledDevelopmentCoordinator;
pub use dependency_exception_match::DependencyExceptionMatch;
pub use dependency_file_classifier::classify_dependency_file;
pub use dependency_file_kind::DependencyFileKind;
pub use owned_workspace_root::ControlledDevelopmentOwnedWorkspaceRoot;
pub use phase::ControlledDevelopmentPhase;
pub use project_state_builder::{
    MAX_PROJECT_STATE_NONBLANK_LINES, MAX_SYSTEM_MAP_COMPONENTS, build_project_state,
};
pub use project_state_input::ControlledDevelopmentProjectStateInput;
pub use promotion_plan::PROJECT_STATE_PATH;
pub use retained_workspace::ControlledDevelopmentRetainedWorkspace;
pub use retained_workspace_reference::ControlledDevelopmentRetainedWorkspaceReference;
pub use service_command::ControlledDevelopmentCommand;
pub use service_effect::ControlledDevelopmentEffect;
pub use session_record::ControlledDevelopmentSessionRecord;
pub use state::ControlledDevelopmentState;
pub use system_map_component::ControlledDevelopmentSystemMapComponent;
pub use transition_error::ControlledDevelopmentTransitionError;
pub use work_card::{
    MAX_COMPLEXITY_EXCEPTIONS, MAX_EXCLUSIONS, MAX_PRODUCTION_PATHS, MAX_PROOF_COMMAND_CHARS,
    MAX_PROOF_COMMANDS, MAX_SUPPORTING_PATHS, MAX_WORK_CARD_ID_CHARS, MAX_WORK_CARD_ITEM_CHARS,
    MAX_WORK_CARD_OUTCOME_CHARS, MAX_WORK_CARD_PATH_CHARS, WorkCard,
};
pub use work_card_schema::work_card_json_schema;
pub use work_card_validation_error::WorkCardValidationError;
pub use work_card_validation_errors::WorkCardValidationErrors;
