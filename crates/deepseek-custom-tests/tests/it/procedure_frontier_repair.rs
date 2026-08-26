use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use deepseek_custom::config::settings::{BackendConfig, ProcedureSettings, Settings};
use deepseek_custom::procedure::{
    AttemptDisposition, AttemptFailureEvidence, AttemptState, ContractSelection, FailureDigest,
    FailureDigestErrorCategory, FrontierPatchDraftError, FrontierRepairDispatch,
    FrontierRepairRequest, LocalRepairOutcome, LocalRepairRun, LocalizationAttempt,
    LocalizationTarget, PatchCandidate, PatchPreview, PatchPreviewId, ProcedureAttemptDisposition,
    ProcedureInputFingerprints, ProcedureReviewDisposition, ProcedureRun, ProcedureRunId,
    ProcedureScratchpad, ProcedureStage, ProcedureTask, ProcedureTerminalDisposition,
    PromotionBaseline, ProposalScope, RepairCandidateId, RepairTier, RequirementSlice,
    RouteDecision, RouteOverride, RouteTier, SelectedContractSlice, StoredProcedureReport,
    ValidatedRepairInput, decode_patch_envelope, dispatch_frontier_repair,
};

const PRIOR_CHAT_SENTINEL: &str = "prior-chat-must-not-cross-frontier";
const PRIOR_MODEL_OUTPUT_SENTINEL: &str = "prior-model-output-must-not-cross-frontier";
const RAW_LOG_SENTINEL: &str = "full-raw-log-must-not-cross-frontier";
const SOURCE_SENTINEL: &str = "source-bytes-must-not-cross-frontier";

fn validated_input() -> ValidatedRepairInput {
    let run_id = ProcedureRunId::new();
    let task = ProcedureTask {
        id: "4.1".to_string(),
        text: "Escalate the same repair task".to_string(),
        covers: Some(
            "deepseek-custom/bounded-repair-escalation :: Exhausted local work escalates the same task :: Frontier receives accumulated evidence"
                .to_string(),
        ),
    };
    let scratchpad = ProcedureScratchpad {
        goals: vec!["Repair the selected behavior.".to_string()],
        files: vec!["src/lib.rs".to_string(), "src/z.rs".to_string()],
        changes: vec!["Keep the public contract.".to_string()],
        last_error: Some("Use deterministic verifier evidence.".to_string()),
    };
    ValidatedRepairInput {
        report: StoredProcedureReport {
            run: ProcedureRun {
                id: run_id,
                change_id: "bounded-change".to_string(),
                selected_task: task.clone(),
                spec_fingerprint: Some("sha256:spec".to_string()),
                repository_fingerprint: Some("sha256:repository".to_string()),
                validation: Some(deepseek_custom::procedure::OpenSpecValidation {
                    command: vec!["openspec".to_string()],
                    exit_code: Some(0),
                    stdout: RAW_LOG_SENTINEL.to_string(),
                    stderr: String::new(),
                }),
                scratchpad: scratchpad.clone(),
                stage: ProcedureStage::Finished,
                attempts: vec![LocalizationAttempt {
                    number: 1,
                    backend: "localizer".to_string(),
                    model: PRIOR_MODEL_OUTPUT_SENTINEL.to_string(),
                    disposition: ProcedureAttemptDisposition::Accepted,
                    targets: vec![LocalizationTarget {
                        path: "src/lib.rs".to_string(),
                        symbol: None,
                        evidence: PRIOR_CHAT_SENTINEL.to_string(),
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
            change_id: "bounded-change".to_string(),
            task,
            proposal_scope: ProposalScope {
                why: "A failed verifier needs bounded escalation.".to_string(),
                what_changes: "Escalate with compact evidence.".to_string(),
            },
            selection: ContractSelection::Bound {
                capability: "deepseek-custom/bounded-repair-escalation".to_string(),
                requirement: RequirementSlice {
                    name: "Exhausted local work escalates the same task".to_string(),
                    text: "The system SHALL send the same compact context.".to_string(),
                    scenarios: Vec::new(),
                },
            },
        },
        preview: PatchPreview {
            id: PatchPreviewId::new(),
            localization_run_id: run_id,
            change_id: "bounded-change".to_string(),
            task_id: "4.1".to_string(),
            route: RouteDecision {
                automatic_tier: RouteTier::Local,
                effective_tier: RouteTier::Local,
                signals: Vec::new(),
                selected_override: RouteOverride::Automatic,
                overridden: false,
            },
            backend: "local".to_string(),
            model: "local-model".to_string(),
            targets: vec![
                "src\\lib.rs".to_string(),
                "src/z.rs".to_string(),
                "src/lib.rs".to_string(),
            ],
            rationale: PRIOR_MODEL_OUTPUT_SENTINEL.to_string(),
            unified_diff: SOURCE_SENTINEL.to_string(),
        },
        promotion_baseline: PromotionBaseline::from_fingerprints(Vec::new()),
    }
}

fn policy() -> deepseek_custom::config::settings::ValidatedProcedureRepairPolicy {
    Settings {
        procedure: Some(ProcedureSettings {
            local_verifier_attempts: 3,
            frontier_patch_backend: Some("configured-frontier".to_string()),
            frontier_attempts: 2,
            ..ProcedureSettings::default()
        }),
        backends: Some(HashMap::from([(
            "configured-frontier".to_string(),
            BackendConfig::CodexCli {
                model: "frontier-model".to_string(),
                sandbox: Some("workspace-write".to_string()),
                env: None,
                models: None,
            },
        )])),
        ..Settings::default()
    }
    .validated_procedure_repair_policy()
    .unwrap()
}

fn exhaust_local(input: &ValidatedRepairInput) -> (AttemptState, Vec<FailureDigest>) {
    let mut state = AttemptState::from_validated_input(input, policy());
    let mut digests = Vec::new();
    for attempt in 1..=3 {
        state
            .start_local_candidate(RepairCandidateId::new(format!("local-{attempt}")).unwrap())
            .unwrap();
        let diagnostic = format!("deterministic diagnostic {attempt}");
        state
            .local_verifier_failure(AttemptFailureEvidence::verifier(
                format!("cargo test --attempt {attempt}"),
                Some(100 + i32::from(attempt)),
                diagnostic.clone(),
            ))
            .unwrap();
        digests.push(FailureDigest {
            attempt_number: attempt,
            tier: RepairTier::Local,
            command: format!("cargo test --attempt {attempt}"),
            exit_code: Some(100 + i32::from(attempt)),
            error_category: FailureDigestErrorCategory::VerifierFailed,
            diagnostic,
        });
    }
    (state, digests)
}

fn frontier_candidate() -> PatchCandidate {
    decode_patch_envelope(
        &serde_json::json!({
            "targets": ["src/lib.rs"],
            "rationale": "Frontier repair candidate.",
            "route": {
                "automatic_tier": "frontier",
                "effective_tier": "frontier",
                "signals": [{"kind": "substantive_logic"}],
                "selected_override": "automatic",
                "overridden": false
            },
            "unified_diff": "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-pub fn old() {}\n+pub fn repaired() {}\n"
        })
        .to_string(),
    )
    .unwrap()
}

#[derive(Default)]
struct ScriptedFrontierDispatcher {
    requests: Mutex<Vec<FrontierRepairRequest>>,
}

#[async_trait]
impl FrontierRepairDispatch for ScriptedFrontierDispatcher {
    async fn draft(
        &self,
        request: &FrontierRepairRequest,
    ) -> Result<PatchCandidate, FrontierPatchDraftError> {
        self.requests.lock().unwrap().push(request.clone());
        Ok(frontier_candidate())
    }
}

// covers: deepseek-custom/bounded-repair-escalation :: Exhausted local work escalates the same task :: Frontier receives accumulated evidence
#[test]
fn local_exhaustion_dispatches_the_same_compact_context_to_configured_frontier() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let input = validated_input();
        let (state, digests) = exhaust_local(&input);
        let mut local_run = LocalRepairRun {
            repair_input: input.clone(),
            state,
            failure_digests: digests.clone(),
            outcome: LocalRepairOutcome::LocalExhausted,
        };
        let dispatcher = ScriptedFrontierDispatcher::default();

        let result = dispatch_frontier_repair(&mut local_run, &dispatcher)
            .await
            .unwrap();

        assert_eq!(local_run.state.tier(), RepairTier::Frontier);
        assert_eq!(local_run.state.attempt_index(), 1);
        assert_eq!(
            local_run.state.disposition(),
            &AttemptDisposition::CandidateActive
        );
        assert_eq!(dispatcher.requests.lock().unwrap().len(), 1);
        assert_eq!(result.request.backend(), "configured-frontier");
        assert_eq!(result.request.change_id(), input.contract.change_id);
        assert_eq!(result.request.task(), &input.contract.task);
        assert_eq!(result.request.spec_slice(), &input.contract.selection);
        assert_eq!(result.request.targets(), ["src/lib.rs", "src/z.rs"]);
        assert_eq!(result.request.scratchpad(), &input.report.run.scratchpad);
        assert_eq!(result.request.failure_digests(), digests);
        assert_eq!(result.candidate.envelope().targets, ["src/lib.rs"]);

        let prompt = result.request.prompt();
        for attempt in 1..=3 {
            assert!(prompt.contains(&format!("attempt={attempt}")));
            assert!(prompt.contains(&format!("cargo test --attempt {attempt}")));
            assert!(prompt.contains(&format!("exit_code={}", 100 + attempt)));
        }
        assert!(prompt.contains("deterministic diagnostic 3"));
        for forbidden in [
            PRIOR_CHAT_SENTINEL,
            PRIOR_MODEL_OUTPUT_SENTINEL,
            RAW_LOG_SENTINEL,
            SOURCE_SENTINEL,
        ] {
            assert!(
                !prompt.contains(forbidden),
                "frontier prompt leaked {forbidden}"
            );
        }
    });
}
