use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use async_trait::async_trait;
use deepseek_custom::config::settings::{BackendConfig, ProcedureSettings, Settings};
use deepseek_custom::procedure::{
    AttemptDisposition, AttemptFailureEvidence, AttemptState, BoundedVerifierOutput,
    ContractSelection, GitApplyDisposition, GitApplyPhase, GitApplyResult, LocalPatchDraftDispatch,
    LocalPatchDraftError, LocalStructuralRepairOutcome, PatchApplyCheckError, PatchCandidate,
    PatchEnvelopeError, PatchPreview, PatchPreviewId, ProcedureInputFingerprints,
    ProcedureReviewDisposition, ProcedureRun, ProcedureRunId, ProcedureScratchpad, ProcedureStage,
    ProcedureTask, ProcedureTerminalDisposition, PromotionBaseline, ProposalScope,
    RepairCandidateId, RepairFailureRef, RepairTier, RequirementSlice, RouteDecision,
    RouteOverride, RouteTier, SelectedContractSlice, StoredProcedureReport,
    StructuralFailureCategory, ValidatedRepairInput, classify_structural_failure,
    decode_patch_envelope, draft_local_with_structural_retry, validate_patch_boundary,
};

const VALID_DIFF: &str = concat!(
    "diff --git a/src/lib.rs b/src/lib.rs\n",
    "--- a/src/lib.rs\n",
    "+++ b/src/lib.rs\n",
    "@@ -1 +1 @@\n",
    "-old\n",
    "+new\n",
);

fn envelope(diff: &str, target: &str) -> String {
    serde_json::json!({
        "targets": [target],
        "rationale": "Repair the selected target.",
        "route": {
            "automatic_tier": "local",
            "effective_tier": "local",
            "signals": [],
            "selected_override": "automatic",
            "overridden": false
        },
        "unified_diff": diff
    })
    .to_string()
}

fn empty_output() -> BoundedVerifierOutput {
    BoundedVerifierOutput {
        text: String::new(),
        first_edge: String::new(),
        last_edge: String::new(),
        truncated: false,
        bytes_seen: 0,
    }
}

fn rejected_patch_parse() -> PatchApplyCheckError {
    PatchApplyCheckError::Rejected {
        result: Box::new(GitApplyResult {
            phase: GitApplyPhase::Check,
            command: "git apply --check".to_string(),
            disposition: GitApplyDisposition::Rejected,
            success: false,
            status_code: Some(128),
            stdout: empty_output(),
            stderr: empty_output(),
            combined_output: empty_output(),
            duration_millis: 1,
            error: None,
        }),
        diagnostics: "error: corrupt patch at line 7".to_string(),
    }
}

#[test]
fn structural_failure_table_pins_categories_and_exact_diagnostics() {
    let schema = PatchEnvelopeError::Structure {
        reason: "missing field `targets`".to_string(),
    };
    let envelope_error = PatchEnvelopeError::Json {
        message: "expected value".to_string(),
        line: 1,
        column: 1,
    };
    let invalid_allowlist = validate_patch_boundary(
        decode_patch_envelope(&envelope(VALID_DIFF, "src/lib.rs")).unwrap(),
        ["src/other.rs"],
    )
    .unwrap_err();
    let unified_diff = PatchEnvelopeError::Diff {
        reason: "first line must start with `diff --git `".to_string(),
    };
    let patch_parse = rejected_patch_parse();
    let cases = [
        (
            RepairFailureRef::Envelope(&schema),
            StructuralFailureCategory::Schema,
            "patch envelope is structurally invalid: missing field `targets`",
        ),
        (
            RepairFailureRef::Envelope(&envelope_error),
            StructuralFailureCategory::Envelope,
            "patch output is not exactly one JSON document: expected value at line 1 column 1",
        ),
        (
            RepairFailureRef::LocalizationAllowlist(&invalid_allowlist),
            StructuralFailureCategory::Allowlist,
            concat!(
                "patch violates the localization boundary:\n",
                "- unexpected path `src/lib.rs` is outside the localization allowlist"
            ),
        ),
        (
            RepairFailureRef::Envelope(&unified_diff),
            StructuralFailureCategory::PatchParse,
            "patch envelope unified_diff is invalid: first line must start with `diff --git `",
        ),
        (
            RepairFailureRef::PatchApply(&patch_parse),
            StructuralFailureCategory::PatchParse,
            "patch hunks do not apply to the current source snapshot: error: corrupt patch at line 7",
        ),
    ];

    for (source, expected_category, expected_diagnostic) in cases {
        let failure = classify_structural_failure(source).unwrap();
        assert_eq!(failure.category(), expected_category);
        assert_eq!(failure.category().to_string(), expected_category.name());
        assert_eq!(failure.diagnostic(), expected_diagnostic);
    }
}

#[test]
fn transport_cancellation_and_verifier_failures_are_not_structural() {
    let transport = LocalPatchDraftError::Request {
        backend: "local".to_string(),
        reason: "connection reset".to_string(),
    };
    let unsupported = LocalPatchDraftError::UnsupportedBackend {
        backend: "frontier".to_string(),
        description: "kind codex_cli".to_string(),
    };
    let sources = [
        RepairFailureRef::LocalDraft(&transport),
        RepairFailureRef::LocalDraft(&unsupported),
        RepairFailureRef::Cancellation,
        RepairFailureRef::VerifierCommandFailure,
    ];

    for source in sources {
        assert!(classify_structural_failure(source).is_none());
    }
}

#[test]
fn missing_final_content_is_an_exact_envelope_failure() {
    let error = LocalPatchDraftError::MissingFinalContent;
    let failure = classify_structural_failure(RepairFailureRef::LocalDraft(&error)).unwrap();

    assert_eq!(failure.category(), StructuralFailureCategory::Envelope);
    assert_eq!(
        failure.diagnostic(),
        "local patch response has no plain final content"
    );
}

enum DraftStep {
    Output(String),
}

struct ScriptedLocalDraft {
    steps: Mutex<VecDeque<DraftStep>>,
    prompts: Mutex<Vec<String>>,
}

impl ScriptedLocalDraft {
    fn new(steps: impl IntoIterator<Item = DraftStep>) -> Self {
        Self {
            steps: Mutex::new(steps.into_iter().collect()),
            prompts: Mutex::new(Vec::new()),
        }
    }

    fn prompts(&self) -> Vec<String> {
        self.prompts.lock().unwrap().clone()
    }
}

#[async_trait]
impl LocalPatchDraftDispatch for ScriptedLocalDraft {
    fn backend_name(&self) -> &str {
        "scripted-local"
    }

    fn model(&self) -> &str {
        "scripted-model"
    }

    async fn draft(&self, prompt: String) -> Result<PatchCandidate, LocalPatchDraftError> {
        self.prompts.lock().unwrap().push(prompt);
        match self
            .steps
            .lock()
            .unwrap()
            .pop_front()
            .expect("scripted local draft received an unexpected extra call")
        {
            DraftStep::Output(output) => decode_patch_envelope(&output)
                .map_err(|source| LocalPatchDraftError::InvalidEnvelope { source }),
        }
    }
}

fn valid_patch_output() -> String {
    envelope(VALID_DIFF, "src/lib.rs")
}

fn candidate_id(value: &str) -> RepairCandidateId {
    RepairCandidateId::new(value).unwrap()
}

fn validated_repair_input() -> ValidatedRepairInput {
    let run_id = ProcedureRunId::new();
    let task = ProcedureTask {
        id: "2.2".to_string(),
        text: "Retry the structural patch failure".to_string(),
        covers: None,
    };
    ValidatedRepairInput {
        report: StoredProcedureReport {
            run: ProcedureRun {
                id: run_id,
                change_id: "change".to_string(),
                selected_task: task.clone(),
                spec_fingerprint: Some("spec".to_string()),
                repository_fingerprint: Some("repository".to_string()),
                validation: None,
                scratchpad: ProcedureScratchpad::default(),
                stage: ProcedureStage::Finished,
                attempts: Vec::new(),
                review_disposition: ProcedureReviewDisposition::Approved,
                terminal_disposition: Some(ProcedureTerminalDisposition::AwaitingReview),
            },
            input_fingerprints: ProcedureInputFingerprints::default(),
            verification: None,
            repair_events: Vec::new(),
        },
        contract: SelectedContractSlice {
            change_id: "change".to_string(),
            task,
            proposal_scope: ProposalScope {
                why: String::new(),
                what_changes: String::new(),
            },
            selection: ContractSelection::Bound {
                capability: "fixture".to_string(),
                requirement: RequirementSlice {
                    name: "Repair".to_string(),
                    text: "Repair the target.".to_string(),
                    scenarios: Vec::new(),
                },
            },
        },
        preview: PatchPreview {
            id: PatchPreviewId::new(),
            localization_run_id: run_id,
            change_id: "change".to_string(),
            task_id: "2.2".to_string(),
            route: RouteDecision {
                automatic_tier: RouteTier::Local,
                effective_tier: RouteTier::Local,
                signals: Vec::new(),
                selected_override: RouteOverride::Automatic,
                overridden: false,
            },
            backend: "scripted-local".to_string(),
            model: "scripted-model".to_string(),
            targets: vec!["src/lib.rs".to_string()],
            rationale: "fixture".to_string(),
            unified_diff: VALID_DIFF.to_string(),
        },
        promotion_baseline: PromotionBaseline::from_fingerprints(Vec::new()),
    }
}

fn repair_state(frontier_attempts: u8) -> AttemptState {
    let frontier_backend = (frontier_attempts > 0).then(|| "frontier".to_string());
    let backends = frontier_backend.as_ref().map(|name| {
        HashMap::from([(
            name.clone(),
            BackendConfig::CodexCli {
                model: "gpt-5".to_string(),
                sandbox: Some("workspace-write".to_string()),
                env: None,
                models: None,
            },
        )])
    });
    let policy = Settings {
        procedure: Some(ProcedureSettings {
            frontier_patch_backend: frontier_backend,
            structural_retries: 1,
            local_verifier_attempts: 3,
            frontier_attempts,
            ..ProcedureSettings::default()
        }),
        backends,
        ..Settings::default()
    }
    .validated_procedure_repair_policy()
    .unwrap();
    AttemptState::from_validated_input(&validated_repair_input(), policy)
}

fn run_async(future: impl std::future::Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future);
}

// covers: deepseek-custom/bounded-repair-escalation :: Structural failures receive one local retry :: Parser retry succeeds
#[test]
fn one_structural_retry_uses_fresh_task_context_and_returns_a_boundary_checked_candidate() {
    run_async(async {
        const TASK_CONTEXT: &str = "{\"change_id\":\"change\",\"task_id\":\"2.2\",\"task\":\"Retry the structural patch failure\"}";
        const PRIOR_HISTORY: &str = "PRIOR_PROMPT_OR_CHAT_HISTORY_MUST_NOT_APPEAR";
        let dispatcher = ScriptedLocalDraft::new([
            DraftStep::Output("not-json".to_string()),
            DraftStep::Output(valid_patch_output()),
        ]);
        let mut state = repair_state(2);
        let allowlist = vec!["src/lib.rs".to_string()];

        let outcome = draft_local_with_structural_retry(
            &dispatcher,
            &mut state,
            TASK_CONTEXT,
            &allowlist,
            candidate_id("local-1"),
            candidate_id("local-1-structural-retry"),
        )
        .await
        .unwrap();

        let LocalStructuralRepairOutcome::ReadyForVerification(candidate) = outcome else {
            panic!("valid structural retry did not continue to verification");
        };
        assert_eq!(candidate.paths(), ["src/lib.rs"]);
        assert_eq!(candidate.file_count(), 1);
        assert_eq!(state.tier(), RepairTier::Local);
        assert_eq!(state.attempt_index(), 1);
        assert_eq!(state.structural_retry_count(), 1);
        assert_eq!(state.disposition(), &AttemptDisposition::CandidateActive);
        assert_eq!(
            state.last_candidate().map(RepairCandidateId::as_str),
            Some("local-1-structural-retry")
        );
        assert_eq!(state.failures().len(), 1);
        assert_eq!(state.failures()[0].attempt_index(), 1);
        assert_eq!(state.failures()[0].candidate().as_str(), "local-1");
        assert_eq!(
            state.failures()[0].evidence().diagnostic(),
            "patch output is not exactly one JSON document: expected ident at line 1 column 2"
        );

        let prompts = dispatcher.prompts();
        assert_eq!(prompts.len(), 2);
        assert_eq!(prompts[0], TASK_CONTEXT);
        assert_eq!(
            prompts[1],
            concat!(
                "{\"change_id\":\"change\",\"task_id\":\"2.2\",\"task\":\"Retry the structural patch failure\"}",
                "\n\nStructural failure diagnostic:\n",
                "patch output is not exactly one JSON document: expected ident at line 1 column 2",
                "\n\nReturn one corrected patch envelope for the same task. Do not include commentary or prior conversation."
            )
        );
        assert_eq!(prompts[1].matches(TASK_CONTEXT).count(), 1);
        assert_eq!(
            prompts[1]
                .matches(state.failures()[0].evidence().diagnostic())
                .count(),
            1
        );
        assert!(!prompts[1].contains(PRIOR_HISTORY));
    });
}

// covers: deepseek-custom/bounded-repair-escalation :: Structural failures receive one local retry :: Parser retry fails again
#[test]
fn second_structural_failure_records_exhaustion_without_a_third_local_request() {
    run_async(async {
        const TASK_CONTEXT: &str = "bounded structural retry task context";
        let outside_diff = concat!(
            "diff --git a/src/outside.rs b/src/outside.rs\n",
            "--- a/src/outside.rs\n",
            "+++ b/src/outside.rs\n",
            "@@ -1 +1 @@\n",
            "-old\n",
            "+new\n",
        );

        for frontier_attempts in [2, 0] {
            let dispatcher = ScriptedLocalDraft::new([
                DraftStep::Output("not-json".to_string()),
                DraftStep::Output(envelope(outside_diff, "src/outside.rs")),
            ]);
            let mut state = repair_state(frontier_attempts);
            let allowlist = vec!["src/lib.rs".to_string()];

            let outcome = draft_local_with_structural_retry(
                &dispatcher,
                &mut state,
                TASK_CONTEXT,
                &allowlist,
                candidate_id("local-1"),
                candidate_id("local-1-structural-retry"),
            )
            .await
            .unwrap();

            assert_eq!(dispatcher.prompts().len(), 2);
            assert_eq!(state.attempt_index(), 1);
            assert_eq!(state.structural_retry_count(), 1);
            assert_eq!(state.failures().len(), 2);
            assert_eq!(state.failures()[0].tier(), RepairTier::Local);
            assert_eq!(state.failures()[0].attempt_index(), 1);
            assert_eq!(state.failures()[0].candidate().as_str(), "local-1");
            assert_eq!(
                state.failures()[0].evidence().structural_category(),
                Some(StructuralFailureCategory::Envelope)
            );
            assert_eq!(
                state.failures()[0].evidence().diagnostic(),
                "patch output is not exactly one JSON document: expected ident at line 1 column 2"
            );
            assert_eq!(state.failures()[1].tier(), RepairTier::Local);
            assert_eq!(state.failures()[1].attempt_index(), 1);
            assert_eq!(
                state.failures()[1].evidence().structural_category(),
                Some(StructuralFailureCategory::Allowlist)
            );
            assert_eq!(
                state.failures()[1].candidate().as_str(),
                "local-1-structural-retry"
            );
            assert_eq!(
                state.failures()[1].evidence().diagnostic(),
                concat!(
                    "patch violates the localization boundary:\n",
                    "- unexpected path `src/outside.rs` is outside the localization allowlist"
                )
            );

            if frontier_attempts > 0 {
                assert_eq!(outcome, LocalStructuralRepairOutcome::EscalationReady);
                assert_eq!(state.tier(), RepairTier::Frontier);
                assert_eq!(state.disposition(), &AttemptDisposition::Ready);
            } else {
                assert_eq!(outcome, LocalStructuralRepairOutcome::Blocked);
                assert_eq!(state.tier(), RepairTier::Local);
                assert_eq!(
                    state.disposition(),
                    &AttemptDisposition::Blocked {
                        reason: "structural retry exhausted and frontier escalation is disabled"
                            .to_string()
                    }
                );
            }

            let exhausted = state.clone();
            assert!(
                state
                    .start_local_candidate(candidate_id("forbidden-local-3"))
                    .is_err()
            );
            assert!(
                state
                    .retry_structural(
                        AttemptFailureEvidence::structural("forbidden third structural failure"),
                        candidate_id("forbidden-structural-3"),
                    )
                    .is_err()
            );
            assert_eq!(state, exhausted);
            assert_eq!(dispatcher.prompts().len(), 2);
        }
    });
}
