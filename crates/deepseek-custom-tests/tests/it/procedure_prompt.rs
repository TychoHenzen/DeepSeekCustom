use deepseek_custom::procedure::{
    ContractSelection, LocalizationPromptInput, ProcedureScratchpad, ProcedureTask, ProposalScope,
    RepositoryIndexEntry, RequirementSlice, ScenarioSlice, SelectedContractSlice,
    TARGET_SELECTION_INSTRUCTION, build_localization_prompt,
};

const UNRELATED_HISTORY_SENTINEL: &str = "UNRELATED_CONVERSATION_8D6F9F33";

fn selected_contract() -> SelectedContractSlice {
    SelectedContractSlice {
        change_id: "add-procedure-localization-runner".to_string(),
        task: ProcedureTask {
            id: "2.3".to_string(),
            text: "Build the typed localization prompt".to_string(),
            covers: Some(
                "deepseek-custom/procedure-localization :: Localization context is short and typed :: Captured request contains only stage context"
                    .to_string(),
            ),
        },
        proposal_scope: ProposalScope {
            why: "A read-only localization stage is needed.".to_string(),
            what_changes: "Add a typed prompt boundary.".to_string(),
        },
        selection: ContractSelection::Bound {
            capability: "deepseek-custom/procedure-localization".to_string(),
            requirement: RequirementSlice {
                name: "Localization context is short and typed".to_string(),
                text: "The system SHALL use only typed stage context.".to_string(),
                scenarios: vec![ScenarioSlice {
                    name: "Captured request contains only stage context".to_string(),
                    text: "- **WHEN** a request is captured\n- **THEN** only stage context appears"
                        .to_string(),
                }],
            },
        },
    }
}

fn repository_index() -> Vec<RepositoryIndexEntry> {
    vec![
        RepositoryIndexEntry {
            path: "crates/deepseek-custom/src/procedure/prompt.rs".to_string(),
            symbols: vec!["build_localization_prompt".to_string()],
        },
        RepositoryIndexEntry {
            path: "docs/IntelligenceProcedure.md".to_string(),
            symbols: Vec::new(),
        },
    ]
}

fn scratchpad() -> ProcedureScratchpad {
    ProcedureScratchpad {
        goals: vec!["localize the selected contract".to_string()],
        files: vec!["crates/deepseek-custom/src/procedure/prompt.rs".to_string()],
        changes: vec!["typed stage context".to_string()],
        last_error: Some("first result used an invented symbol".to_string()),
    }
}

// covers: deepseek-custom/procedure-localization :: Localization context is short and typed :: Captured request contains only stage context
#[test]
fn localization_prompt_serializes_only_the_three_typed_stage_inputs() {
    let contract = selected_contract();
    let repository_index = repository_index();
    let scratchpad = scratchpad();

    let prompt = build_localization_prompt(LocalizationPromptInput {
        contract: &contract,
        repository_index: &repository_index,
        scratchpad: &scratchpad,
    })
    .unwrap();
    let context: serde_json::Value = serde_json::from_str(prompt.split_once("\n\n").unwrap().1)
        .expect("prompt context should be one JSON object");

    let keys = context
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(keys, vec!["contract", "repository_index", "scratchpad"]);
    assert!(prompt.contains("Localization context is short and typed"));
    assert!(prompt.contains("Captured request contains only stage context"));
    assert!(prompt.contains("crates/deepseek-custom/src/procedure/prompt.rs"));
    assert!(prompt.contains("build_localization_prompt"));
    assert!(prompt.contains("localize the selected contract"));
    assert!(prompt.contains("first result used an invented symbol"));
}

// covers: deepseek-custom/procedure-localization :: Localization context is short and typed :: Key instruction is not buried
#[test]
fn target_selection_instruction_is_the_prompt_prefix() {
    let contract = selected_contract();
    let repository_index = repository_index();
    let scratchpad = scratchpad();

    let captured_prompt = build_localization_prompt(LocalizationPromptInput {
        contract: &contract,
        repository_index: &repository_index,
        scratchpad: &scratchpad,
    })
    .unwrap();

    assert!(captured_prompt.starts_with(TARGET_SELECTION_INSTRUCTION));
    assert_eq!(
        captured_prompt
            .matches(TARGET_SELECTION_INSTRUCTION)
            .count(),
        1
    );
}

#[test]
fn captured_prompt_excludes_unrelated_conversation_history() {
    let unrelated_conversation_history = [
        format!("user: {UNRELATED_HISTORY_SENTINEL}"),
        "assistant: unrelated answer".to_string(),
    ];
    let contract = selected_contract();
    let repository_index = repository_index();
    let scratchpad = scratchpad();

    let captured_prompt = build_localization_prompt(LocalizationPromptInput {
        contract: &contract,
        repository_index: &repository_index,
        scratchpad: &scratchpad,
    })
    .unwrap();

    assert!(
        unrelated_conversation_history
            .iter()
            .all(|message| !captured_prompt.contains(message))
    );
    assert!(!captured_prompt.contains(UNRELATED_HISTORY_SENTINEL));
}
