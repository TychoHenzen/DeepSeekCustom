//! Fresh bounded prompts for verifier-driven repair attempts.

use serde::Serialize;
use thiserror::Error;

use super::{
    ContractSelection, FailureDigest, ProcedureScratchpad, ProcedureTask, RepairTier,
    ValidatedRepairInput, build_failure_digest_section,
};

/// Fixed final instruction for every verifier-driven repair request.
pub const REPAIR_INSTRUCTION: &str = "Return one corrected patch envelope for this task. Change only the normalized targets. Do not include commentary or prior conversation.";

/// Every input allowed to reach a verifier-driven repair prompt.
pub struct RepairPromptInput<'a> {
    pub repair: &'a ValidatedRepairInput,
    pub failure_digests: &'a [FailureDigest],
    pub failure_character_cap: usize,
}

#[derive(Serialize)]
struct RepairContext<'a> {
    change_id: &'a str,
    task: &'a ProcedureTask,
    spec_slice: &'a ContractSelection,
    targets: Vec<String>,
    scratchpad: &'a ProcedureScratchpad,
}

/// Failure while serializing the typed prompt boundary.
#[derive(Debug, Error)]
pub enum RepairPromptError {
    #[error(transparent)]
    FailureSection(#[from] super::FailureDigestSectionError),
    #[error("could not serialize repair prompt context: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// Rebuild a prompt without any earlier model message or conversation state.
pub fn build_repair_prompt(input: RepairPromptInput<'_>) -> Result<String, RepairPromptError> {
    let mut targets = input
        .repair
        .preview
        .targets
        .iter()
        .map(|path| path.replace('\\', "/"))
        .collect::<Vec<_>>();
    targets.sort();
    targets.dedup();

    let context = serde_json::to_string_pretty(&RepairContext {
        change_id: &input.repair.contract.change_id,
        task: &input.repair.contract.task,
        spec_slice: &input.repair.contract.selection,
        targets,
        scratchpad: &input.repair.report.run.scratchpad,
    })?;
    let local_failures = input
        .failure_digests
        .iter()
        .filter(|failure| failure.tier == RepairTier::Local)
        .cloned()
        .collect::<Vec<_>>();
    let failures = build_failure_digest_section(&local_failures, input.failure_character_cap)?;

    Ok(format!(
        "Repair context:\n{context}\n\nDeterministic failures:\n{failures}\n\n{REPAIR_INSTRUCTION}"
    ))
}
