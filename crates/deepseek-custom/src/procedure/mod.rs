//! Typed state and services for the staged coding procedure.
//!
//! Procedure runs are separate from conversational history and the search
//! subsystem. Each run carries the complete typed state needed to rebuild a
//! short prompt for its current stage.

mod dispatch;
mod index;
mod input;
mod prompt;
pub mod report;
mod run;
mod runner;
mod schema;
mod validation;

pub use dispatch::{LocalizationDispatch, LocalizationDispatchError, LocalizationDispatcher};
pub use index::{RepositoryIndexError, build_repository_index};
pub use input::{
    CapabilityDeltaSlice, ContractSelection, OpenSpecChange, OpenSpecCommandFailure, OpenSpecInput,
    OpenSpecInputError, OpenSpecValidation, ProposalScope, RequirementSlice, ScenarioSlice,
    SelectedContractSlice, ValidatedContractInput,
};
pub use prompt::{
    LocalizationPromptInput, RepositoryIndexEntry, TARGET_SELECTION_INSTRUCTION,
    build_localization_prompt,
};
pub use report::ProcedureReportStore;

pub use run::{
    LocalizationAttempt, LocalizationEnvelope, LocalizationTarget, ProcedureAttemptDisposition,
    ProcedureRun, ProcedureRunId, ProcedureScratchpad, ProcedureStage, ProcedureTask,
    ProcedureTerminalDisposition,
};
pub use runner::{ProcedureProgress, ProcedureRunRequest, ProcedureRunner, ProcedureRunnerError};
pub use schema::localization_response_format;
pub use validation::{
    LocalizationTargetRejection, LocalizationTargetValidationError, validate_localization_targets,
};
