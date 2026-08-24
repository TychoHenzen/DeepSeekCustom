//! Typed state and services for the staged coding procedure.
//!
//! Procedure runs are separate from conversational history and the search
//! subsystem. Each run carries the complete typed state needed to rebuild a
//! short prompt for its current stage.

pub mod report;
mod run;

pub use report::ProcedureReportStore;

pub use run::{
    LocalizationAttempt, LocalizationTarget, ProcedureAttemptDisposition, ProcedureRun,
    ProcedureRunId, ProcedureScratchpad, ProcedureStage, ProcedureTask,
    ProcedureTerminalDisposition,
};
