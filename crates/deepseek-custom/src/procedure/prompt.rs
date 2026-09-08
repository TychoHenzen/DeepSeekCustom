//! Fresh, typed Stage 1 localization prompts.

use serde::Serialize;

use super::{ProcedureScratchpad, SelectedContractSlice};

/// The key localization rule. It is the first text in every prompt so it
/// cannot be buried between context sections.
pub const TARGET_SELECTION_INSTRUCTION: &str = "Select only repository targets present in repository_index. Return each path exactly as indexed. Include a symbol only when that symbol appears under the same path.";

/// The bounded repository data supplied by the repository index stage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepositoryIndexEntry {
    pub path: String,
    pub symbols: Vec<String>,
}

/// Every input allowed to reach a localization prompt.
///
/// This type has no conversation transcript or message history field. A
/// retry rebuilds it from the selected contract, current index, and typed
/// scratchpad instead of appending earlier model turns.
pub struct LocalizationPromptInput<'a> {
    pub contract: &'a SelectedContractSlice,
    pub repository_index: &'a [RepositoryIndexEntry],
    pub scratchpad: &'a ProcedureScratchpad,
}

#[derive(Serialize)]
struct LocalizationContext<'a> {
    contract: &'a SelectedContractSlice,
    repository_index: &'a [RepositoryIndexEntry],
    scratchpad: &'a ProcedureScratchpad,
}

/// Build one deterministic, tool-free localization prompt.
pub fn build_localization_prompt(
    input: LocalizationPromptInput<'_>,
) -> Result<String, serde_json::Error> {
    let context = serde_json::to_string(&LocalizationContext {
        contract: input.contract,
        repository_index: input.repository_index,
        scratchpad: input.scratchpad,
    })?;
    Ok(format!("{TARGET_SELECTION_INSTRUCTION}\n\n{context}"))
}
