use std::time::Duration;

use deepseek_custom::procedure::{
    BoundedVerifierOutput, ContractSelection, FailureDigest, LocalizationAttempt,
    LocalizationTarget, PatchPreview, PatchPreviewId, ProcedureAttemptDisposition,
    ProcedureInputFingerprints, ProcedureReviewDisposition, ProcedureRun, ProcedureRunId,
    ProcedureScratchpad, ProcedureStage, ProcedureTask, ProcedureTerminalDisposition,
    PromotionBaseline, ProposalScope, REPAIR_INSTRUCTION, RepairPromptInput, RepairTier,
    RequirementSlice, RouteDecision, RouteOverride, RouteTier, ScenarioSlice,
    SelectedContractSlice, StoredProcedureReport, ValidatedRepairInput, VerifierCommandDisposition,
    VerifierCommandResult, build_repair_prompt,
};

const PRIOR_PROMPT: &str = "PRIOR_PROMPT_SENTINEL";
const CHAT_HISTORY: &str = "CHAT_HISTORY_SENTINEL";
const MODEL_OUTPUT: &str = "MODEL_OUTPUT_SENTINEL";
const SOURCE_CONTENTS: &str = "SOURCE_CONTENTS_SENTINEL";
const UNRELATED_SPEC: &str = "UNRELATED_SPEC_SENTINEL";

fn verifier_failure(command: &str, code: i32, diagnostic: &str) -> VerifierCommandResult {
    let output = BoundedVerifierOutput {
        text: diagnostic.to_string(),
        first_edge: diagnostic.to_string(),
        last_edge: diagnostic.to_string(),
        truncated: false,
        bytes_seen: diagnostic.len() as u64,
    };
    VerifierCommandResult {
        command: command.to_string(),
        disposition: VerifierCommandDisposition::Failed,
        success: false,
        exit_code: Some(code),
        stdout: BoundedVerifierOutput {
            text: String::new(),
            first_edge: String::new(),
            last_edge: String::new(),
            truncated: false,
            bytes_seen: 0,
        },
        stderr: output.clone(),
        combined_output: output,
        duration: Duration::from_millis(1),
        error: None,
    }
}

fn repair_input() -> ValidatedRepairInput {
    let run_id = ProcedureRunId::new();
    let task = ProcedureTask {
        id: "3.3".to_string(),
        text: "Promote a passing local repair".to_string(),
        covers: Some(
            "deepseek-custom/bounded-repair-escalation :: Verifier failures have a bounded local repair budget :: Local repair passes within budget"
                .to_string(),
        ),
    };
    ValidatedRepairInput {
        report: StoredProcedureReport {
            run: ProcedureRun {
                id: run_id,
                change_id: "add-bounded-repair-escalation".to_string(),
                selected_task: task.clone(),
                spec_fingerprint: Some("sha256:fixture".to_string()),
                repository_fingerprint: Some("sha256:fixture".to_string()),
                validation: None,
                scratchpad: ProcedureScratchpad {
                    goals: vec!["Promote only after deterministic verification.".to_string()],
                    files: vec!["src\\lib.rs".to_string()],
                    changes: vec!["Fix the selected behavior.".to_string()],
                    last_error: Some("The previous candidate failed its test gate.".to_string()),
                },
                stage: ProcedureStage::Finished,
                attempts: vec![LocalizationAttempt {
                    number: 1,
                    backend: PRIOR_PROMPT.to_string(),
                    model: CHAT_HISTORY.to_string(),
                    disposition: ProcedureAttemptDisposition::Accepted,
                    targets: vec![LocalizationTarget {
                        path: "src/lib.rs".to_string(),
                        symbol: None,
                        evidence: MODEL_OUTPUT.to_string(),
                    }],
                    validation_error: None,
                }],
                review_disposition: ProcedureReviewDisposition::Approved,
                terminal_disposition: Some(ProcedureTerminalDisposition::AwaitingReview),
            },
            input_fingerprints: ProcedureInputFingerprints::default(),
            verification: None,
        },
        contract: SelectedContractSlice {
            change_id: "add-bounded-repair-escalation".to_string(),
            task,
            proposal_scope: ProposalScope {
                why: UNRELATED_SPEC.to_string(),
                what_changes: UNRELATED_SPEC.to_string(),
            },
            selection: ContractSelection::Bound {
                capability: "deepseek-custom/bounded-repair-escalation".to_string(),
                requirement: RequirementSlice {
                    name: "Verifier failures have a bounded local repair budget".to_string(),
                    text: "The system SHALL allow at most three local verifier-driven patch attempts by default."
                        .to_string(),
                    scenarios: vec![ScenarioSlice {
                        name: "Local repair passes within budget".to_string(),
                        text: "- **WHEN** a local repair candidate passes every gate on attempt three or earlier\n- **THEN** it is promoted through the verification gate and no frontier request is made"
                            .to_string(),
                    }],
                },
            },
        },
        preview: PatchPreview {
            id: PatchPreviewId::new(),
            localization_run_id: run_id,
            change_id: "add-bounded-repair-escalation".to_string(),
            task_id: "3.3".to_string(),
            route: RouteDecision {
                automatic_tier: RouteTier::Local,
                effective_tier: RouteTier::Local,
                signals: Vec::new(),
                selected_override: RouteOverride::Automatic,
                overridden: false,
            },
            backend: PRIOR_PROMPT.to_string(),
            model: CHAT_HISTORY.to_string(),
            targets: vec!["src\\lib.rs".to_string(), "src/lib.rs".to_string()],
            rationale: MODEL_OUTPUT.to_string(),
            unified_diff: SOURCE_CONTENTS.to_string(),
        },
        promotion_baseline: PromotionBaseline::from_fingerprints(Vec::new()),
    }
}

#[test]
fn repair_prompt_snapshot_has_only_typed_context_in_fixed_order() {
    let input = repair_input();
    let failures = vec![
        FailureDigest::from_verifier_result(
            2,
            RepairTier::Local,
            &verifier_failure("cargo test second", 2, "SECOND_DETAIL"),
        ),
        FailureDigest::from_verifier_result(
            1,
            RepairTier::Local,
            &verifier_failure("cargo test first", 1, "FIRST_DETAIL"),
        ),
    ];

    let prompt = build_repair_prompt(RepairPromptInput {
        repair: &input,
        failure_digests: &failures,
        failure_character_cap: 4_000,
    })
    .unwrap();

    let expected = format!(
        concat!(
            "Repair context:\n",
            "{{\n",
            "  \"change_id\": \"add-bounded-repair-escalation\",\n",
            "  \"task\": {{\n",
            "    \"id\": \"3.3\",\n",
            "    \"text\": \"Promote a passing local repair\",\n",
            "    \"covers\": \"deepseek-custom/bounded-repair-escalation :: Verifier failures have a bounded local repair budget :: Local repair passes within budget\"\n",
            "  }},\n",
            "  \"spec_slice\": {{\n",
            "    \"binding\": \"bound\",\n",
            "    \"capability\": \"deepseek-custom/bounded-repair-escalation\",\n",
            "    \"requirement\": {{\n",
            "      \"name\": \"Verifier failures have a bounded local repair budget\",\n",
            "      \"text\": \"The system SHALL allow at most three local verifier-driven patch attempts by default.\",\n",
            "      \"scenarios\": [\n",
            "        {{\n",
            "          \"name\": \"Local repair passes within budget\",\n",
            "          \"text\": \"- **WHEN** a local repair candidate passes every gate on attempt three or earlier\\n- **THEN** it is promoted through the verification gate and no frontier request is made\"\n",
            "        }}\n",
            "      ]\n",
            "    }}\n",
            "  }},\n",
            "  \"targets\": [\n",
            "    \"src/lib.rs\"\n",
            "  ],\n",
            "  \"scratchpad\": {{\n",
            "    \"goals\": [\n",
            "      \"Promote only after deterministic verification.\"\n",
            "    ],\n",
            "    \"files\": [\n",
            "      \"src\\\\lib.rs\"\n",
            "    ],\n",
            "    \"changes\": [\n",
            "      \"Fix the selected behavior.\"\n",
            "    ],\n",
            "    \"last_error\": \"The previous candidate failed its test gate.\"\n",
            "  }}\n",
            "}}\n\n",
            "Deterministic failures:\n",
            "Older failures:\n",
            "attempt=1 tier=local category=verifier_failed exit_code=1 command=cargo test first\n\n",
            "Newest failure:\n",
            "attempt=2 tier=local category=verifier_failed exit_code=2 command=cargo test second\n",
            "diagnostic:\n",
            "SECOND_DETAIL\n\n",
            "{}"
        ),
        REPAIR_INSTRUCTION
    );
    assert_eq!(prompt, expected);
    assert!(!prompt.contains("FIRST_DETAIL"));
    assert_eq!(prompt.lines().last(), Some(REPAIR_INSTRUCTION));
}

#[test]
fn repair_prompt_has_no_prior_prompt_chat_model_source_or_unrelated_spec_content() {
    let input = repair_input();
    let prompt = build_repair_prompt(RepairPromptInput {
        repair: &input,
        failure_digests: &[],
        failure_character_cap: 4_000,
    })
    .unwrap();

    for forbidden in [
        PRIOR_PROMPT,
        CHAT_HISTORY,
        MODEL_OUTPUT,
        SOURCE_CONTENTS,
        UNRELATED_SPEC,
    ] {
        assert!(!prompt.contains(forbidden), "leaked {forbidden}");
    }
    assert!(prompt.contains("No prior deterministic failures."));
    assert_eq!(prompt.lines().last(), Some(REPAIR_INSTRUCTION));
}
