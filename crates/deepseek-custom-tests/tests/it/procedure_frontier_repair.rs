use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use deepseek_custom::config::settings::{BackendConfig, ProcedureSettings, Settings};
use deepseek_custom::procedure::{
    AttemptDisposition, AttemptFailureEvidence, AttemptState, ContractSelection,
    DisposableDraftWorkspace, FailureDigest, FailureDigestErrorCategory, FrontierPatchDraftError,
    FrontierRepairDispatch, FrontierRepairOutcome, FrontierRepairRequest, FrontierRepairRunner,
    LocalRepairOutcome, LocalRepairRun, LocalizationAttempt, LocalizationTarget, PatchCandidate,
    PatchPreview, PatchPreviewId, ProcedureAttemptDisposition, ProcedureInputFingerprints,
    ProcedureProgress, ProcedureReviewDisposition, ProcedureRun, ProcedureRunId,
    ProcedureScratchpad, ProcedureStage, ProcedureTask, ProcedureTerminalDisposition,
    PromotionBaseline, PromotionTarget, ProposalScope, RepairCandidateId, RepairTier,
    RequirementSlice, RouteDecision, RouteOverride, RouteTier, SelectedContractSlice,
    StoredProcedureReport, ValidatedRepairInput, decode_patch_envelope, dispatch_frontier_repair,
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
            repair_events: Vec::new(),
            metrics: None,
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
    fn model(&self, _backend: &str) -> String {
        "scripted-frontier-model".to_string()
    }

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
            repair_events: Vec::new(),
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

const WORKSPACE_TARGET: &str = "src/lib.rs";
const ORIGINAL_BYTES: &str = "pub const VALUE: &str = \"original\";\n";

fn temp_dir(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "dsc-frontier-repair-{tag}-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn init_repository(root: &Path) {
    let status = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(root)
        .status()
        .unwrap();
    assert!(status.success(), "git init failed with {status}");
}

fn write_target(root: &Path, contents: &str) {
    let target = root.join(WORKSPACE_TARGET);
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(target, contents).unwrap();
}

fn workspace_frontier_candidate(value: &str) -> PatchCandidate {
    let diff = format!(
        "diff --git a/{WORKSPACE_TARGET} b/{WORKSPACE_TARGET}\n--- a/{WORKSPACE_TARGET}\n+++ b/{WORKSPACE_TARGET}\n@@ -1 +1 @@\n-{ORIGINAL_BYTES}+pub const VALUE: &str = \"{value}\";\n"
    );
    decode_patch_envelope(
        &serde_json::json!({
            "targets": [WORKSPACE_TARGET],
            "rationale": "Repair the selected frontier target.",
            "route": {
                "automatic_tier": "frontier",
                "effective_tier": "frontier",
                "signals": [{"kind": "substantive_logic"}],
                "selected_override": "automatic",
                "overridden": false
            },
            "unified_diff": diff
        })
        .to_string(),
    )
    .unwrap()
}

fn workspace_local_run(root: &Path) -> LocalRepairRun {
    let mut input = validated_input();
    input.preview.targets = vec!["src\\lib.rs".to_string()];
    input.promotion_baseline = PromotionBaseline::capture(
        root,
        &[PromotionTarget::Update {
            path: WORKSPACE_TARGET.to_string(),
        }],
    )
    .unwrap();
    let (state, failure_digests) = exhaust_local(&input);
    LocalRepairRun {
        repair_input: input,
        state,
        failure_digests,
        repair_events: Vec::new(),
        outcome: LocalRepairOutcome::LocalExhausted,
    }
}

struct QueuedFrontierDispatcher {
    candidates: Mutex<VecDeque<PatchCandidate>>,
    requests: Mutex<Vec<FrontierRepairRequest>>,
    calls: AtomicUsize,
    real_target: PathBuf,
    real_bytes_seen_at_dispatch: Mutex<Vec<Vec<u8>>>,
}

struct InterruptibleFrontierDispatcher {
    root: PathBuf,
    started: PathBuf,
    completed: PathBuf,
    calls: AtomicUsize,
    cancelled: Arc<AtomicBool>,
    disposable_paths: Mutex<Vec<PathBuf>>,
}

struct DispatchCancellationGuard(Arc<AtomicBool>);

impl Drop for DispatchCancellationGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[async_trait]
impl FrontierRepairDispatch for InterruptibleFrontierDispatcher {
    fn model(&self, _backend: &str) -> String {
        "interruptible-frontier-model".to_string()
    }

    async fn draft(
        &self,
        _request: &FrontierRepairRequest,
    ) -> Result<PatchCandidate, FrontierPatchDraftError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let disposable = DisposableDraftWorkspace::create_current_state(&self.root)?;
        self.disposable_paths
            .lock()
            .unwrap()
            .push(disposable.path().to_path_buf());
        let _cancelled = DispatchCancellationGuard(Arc::clone(&self.cancelled));
        let script = if cfg!(windows) {
            self.root.join("interruptible-frontier.ps1")
        } else {
            self.root.join("interruptible-frontier.sh")
        };
        let mut command = if cfg!(windows) {
            let mut command = tokio::process::Command::new("powershell.exe");
            command.args([
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
                &script.display().to_string(),
            ]);
            command
        } else {
            tokio::process::Command::new(&script)
        };
        command.current_dir(&self.root).kill_on_drop(true);
        deepseek_custom::process_group::prepare(&mut command);
        let mut child = command
            .spawn()
            .map_err(|error| FrontierPatchDraftError::Dispatch {
                backend: "fake-frontier".to_string(),
                message: error.to_string(),
            })?;
        deepseek_custom::process_group::adopt(&child);
        let status = child
            .wait()
            .await
            .map_err(|error| FrontierPatchDraftError::Dispatch {
                backend: "fake-frontier".to_string(),
                message: error.to_string(),
            })?;
        if !status.success() {
            return Err(FrontierPatchDraftError::Dispatch {
                backend: "fake-frontier".to_string(),
                message: format!("interrupt fixture exited with {status}"),
            });
        }
        Err(FrontierPatchDraftError::Dispatch {
            backend: "fake-frontier".to_string(),
            message: format!(
                "interrupt fixture unexpectedly completed: {} / {}",
                self.started.display(),
                self.completed.display()
            ),
        })
    }
}

fn write_interruptible_frontier_dispatch(root: &Path, started: &Path, completed: &Path) {
    #[cfg(windows)]
    {
        let quote = |path: &Path| path.display().to_string().replace('\'', "''");
        std::fs::write(
            root.join("interruptible-frontier.ps1"),
            format!(
                "Set-Content -LiteralPath '{}' -Value 'started'\nStart-Sleep -Seconds 4\nSet-Content -LiteralPath '{}' -Value 'child-completed'\n",
                quote(started),
                quote(completed),
            ),
        )
        .unwrap();
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        let quote = |path: &Path| path.display().to_string().replace('\'', "'\\''");
        let script = root.join("interruptible-frontier.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf started > '{}'\nsleep 4\nprintf child-completed > '{}'\n",
                quote(started),
                quote(completed),
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(script, permissions).unwrap();
    }
}

impl QueuedFrontierDispatcher {
    fn new(root: &Path, candidates: Vec<PatchCandidate>) -> Self {
        Self {
            candidates: Mutex::new(candidates.into()),
            requests: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
            real_target: root.join(WORKSPACE_TARGET),
            real_bytes_seen_at_dispatch: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl FrontierRepairDispatch for QueuedFrontierDispatcher {
    fn model(&self, _backend: &str) -> String {
        "queued-frontier-model".to_string()
    }

    async fn draft(
        &self,
        request: &FrontierRepairRequest,
    ) -> Result<PatchCandidate, FrontierPatchDraftError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().unwrap().push(request.clone());
        self.real_bytes_seen_at_dispatch
            .lock()
            .unwrap()
            .push(std::fs::read(&self.real_target).unwrap());
        self.candidates
            .lock()
            .unwrap()
            .pop_front()
            .ok_or(FrontierPatchDraftError::InvalidOutput(
                deepseek_custom::procedure::PatchEnvelopeError::Structure {
                    reason: "unexpected frontier dispatch beyond the configured candidates"
                        .to_string(),
                },
            ))
    }
}

fn write_passing_verifier(root: &Path) -> String {
    #[cfg(windows)]
    {
        let command = root.join("frontier-passing-verifier.ps1");
        let real_target = root
            .join(WORKSPACE_TARGET)
            .display()
            .to_string()
            .replace('\'', "''");
        std::fs::write(
            &command,
            format!(
                "$real = Get-Content -Raw -LiteralPath '{real_target}'\nif (-not $real.Contains('original')) {{ Write-Error 'real workspace changed before gates passed'; exit 41 }}\n$candidate = Get-Content -Raw -LiteralPath 'src/lib.rs'\nif (-not $candidate.Contains('passing')) {{ Write-Error 'candidate did not contain passing'; exit 42 }}\nexit 0\n"
            ),
        )
        .unwrap();
        format!(
            "powershell.exe -NoProfile -ExecutionPolicy Bypass -File \"{}\"",
            command.display()
        )
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        let command = root.join("frontier-passing-verifier.sh");
        let real_target = root
            .join(WORKSPACE_TARGET)
            .display()
            .to_string()
            .replace('\'', "'\\''");
        std::fs::write(
            &command,
            format!(
                "#!/bin/sh\ngrep -q original '{real_target}' || {{ echo 'real workspace changed before gates passed' >&2; exit 41; }}\ngrep -q passing src/lib.rs || {{ echo 'candidate did not contain passing' >&2; exit 42; }}\n"
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&command).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&command, permissions).unwrap();
        format!("\"{}\"", command.display())
    }
}

fn write_failing_isolation_verifier(
    evidence_root: &Path,
    real_root: &Path,
    counter: &Path,
    workspaces: &Path,
) -> String {
    #[cfg(windows)]
    {
        let command = evidence_root.join("frontier-failing-verifier.ps1");
        let quote = |path: &Path| path.display().to_string().replace('\'', "''");
        std::fs::write(
            &command,
            format!(
                "$counterPath = '{}'\n$workspacesPath = '{}'\n$realTarget = '{}'\n$count = if (Test-Path -LiteralPath $counterPath) {{ [int](Get-Content -Raw -LiteralPath $counterPath) }} else {{ 0 }}\n$count += 1\nSet-Content -NoNewline -LiteralPath $counterPath -Value $count\nAdd-Content -LiteralPath $workspacesPath -Value (Get-Location).Path\nif (Test-Path -LiteralPath 'frontier-poison.txt') {{ Write-Error 'prior frontier residue leaked'; exit 51 }}\n$expected = @('first', 'second')[$count - 1]\n$candidate = Get-Content -Raw -LiteralPath 'src/lib.rs'\nif (-not $candidate.Contains($expected)) {{ Write-Error \"expected fresh frontier candidate $expected\"; exit 52 }}\n$real = Get-Content -Raw -LiteralPath $realTarget\nif (-not $real.Contains('original')) {{ Write-Error 'real workspace changed after failed frontier attempt'; exit 53 }}\nSet-Content -LiteralPath 'frontier-poison.txt' -Value 'failed frontier residue'\nSet-Content -LiteralPath 'src/lib.rs' -Value 'verifier-mutated frontier residue'\nWrite-Error \"frontier deterministic failure $count\"\nexit 7\n",
                quote(counter),
                quote(workspaces),
                quote(&real_root.join(WORKSPACE_TARGET)),
            ),
        )
        .unwrap();
        format!(
            "powershell.exe -NoProfile -ExecutionPolicy Bypass -File \"{}\"",
            command.display()
        )
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        let command = evidence_root.join("frontier-failing-verifier.sh");
        let quote = |path: &Path| path.display().to_string().replace('\'', "'\\''");
        std::fs::write(
            &command,
            format!(
                "#!/bin/sh\ncounter='{}'\nworkspaces='{}'\nreal_target='{}'\ncount=0\n[ ! -f \"$counter\" ] || count=$(cat \"$counter\")\ncount=$((count + 1))\nprintf '%s' \"$count\" > \"$counter\"\npwd >> \"$workspaces\"\n[ ! -f frontier-poison.txt ] || {{ echo 'prior frontier residue leaked' >&2; exit 51; }}\ncase $count in 1) expected=first ;; 2) expected=second ;; *) echo 'unexpected frontier attempt' >&2; exit 54 ;; esac\ngrep -q \"$expected\" src/lib.rs || {{ echo \"expected fresh frontier candidate $expected\" >&2; exit 52; }}\ngrep -q original \"$real_target\" || {{ echo 'real workspace changed after failed frontier attempt' >&2; exit 53; }}\nprintf '%s\n' 'failed frontier residue' > frontier-poison.txt\nprintf '%s\n' 'verifier-mutated frontier residue' > src/lib.rs\necho \"frontier deterministic failure $count\" >&2\nexit 7\n",
                quote(counter),
                quote(workspaces),
                quote(&real_root.join(WORKSPACE_TARGET)),
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&command).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&command, permissions).unwrap();
        format!("\"{}\"", command.display())
    }
}

// covers: deepseek-custom/bounded-repair-escalation :: Frontier repair is also bounded :: Frontier candidate passes
#[test]
fn passing_frontier_candidate_uses_shared_gates_then_promotes_exact_bytes() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let root = temp_dir("passing");
        init_repository(&root);
        write_target(&root, ORIGINAL_BYTES);
        let mut local_run = workspace_local_run(&root);
        let dispatcher =
            QueuedFrontierDispatcher::new(&root, vec![workspace_frontier_candidate("passing")]);
        let verifier = write_passing_verifier(&root);

        let outcome = FrontierRepairRunner::new(root.clone())
            .run(&mut local_run, &dispatcher, &[verifier])
            .await
            .unwrap();

        match outcome {
            FrontierRepairOutcome::Promoted {
                attempt_number: 1,
                promotion,
            } => {
                assert!(promotion.baseline.is_current());
                assert_eq!(promotion.baseline.checked_paths, [WORKSPACE_TARGET]);
                assert!(promotion.cleanup.completed());
            }
            other => panic!("unexpected frontier outcome: {other:?}"),
        }
        assert_eq!(local_run.state.disposition(), &AttemptDisposition::Promoted);
        assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 1);
        assert_eq!(local_run.failure_digests.len(), 3);
        assert!(
            dispatcher
                .real_bytes_seen_at_dispatch
                .lock()
                .unwrap()
                .iter()
                .all(|bytes| bytes == ORIGINAL_BYTES.as_bytes())
        );
        assert_eq!(
            std::fs::read(root.join(WORKSPACE_TARGET)).unwrap(),
            b"pub const VALUE: &str = \"passing\";\n"
        );

        std::fs::remove_dir_all(root).ok();
    });
}

// covers: deepseek-custom/bounded-repair-escalation :: Frontier repair is also bounded :: Frontier budget is exhausted
#[test]
fn two_failed_frontier_candidates_block_without_third_dispatch_or_workspace_change() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let root = temp_dir("blocked");
        let evidence = temp_dir("evidence");
        init_repository(&root);
        write_target(&root, ORIGINAL_BYTES);
        let counter = evidence.join("counter.txt");
        let workspaces = evidence.join("workspaces.txt");
        let verifier = write_failing_isolation_verifier(&evidence, &root, &counter, &workspaces);
        let mut local_run = workspace_local_run(&root);
        let dispatcher = QueuedFrontierDispatcher::new(
            &root,
            vec![
                workspace_frontier_candidate("first"),
                workspace_frontier_candidate("second"),
            ],
        );
        let (progress_tx, mut progress_rx) = tokio::sync::mpsc::unbounded_channel();

        let outcome = FrontierRepairRunner::new(root.clone())
            .with_progress(progress_tx)
            .run(&mut local_run, &dispatcher, &[verifier])
            .await
            .unwrap();

        assert!(matches!(
            outcome,
            FrontierRepairOutcome::Blocked { attempts: 2, .. }
        ));
        assert!(matches!(
            local_run.state.disposition(),
            AttemptDisposition::Blocked { reason }
                if reason == "frontier repair exhausted after 2 deterministic verifier attempts"
        ));
        assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 2);
        assert_eq!(local_run.failure_digests.len(), 5);
        assert_eq!(
            local_run
                .failure_digests
                .iter()
                .filter(|digest| digest.tier == RepairTier::Frontier)
                .map(|digest| digest.attempt_number)
                .collect::<Vec<_>>(),
            [1, 2]
        );
        assert!(
            local_run
                .failure_digests
                .iter()
                .filter(|digest| digest.tier == RepairTier::Frontier)
                .all(|digest| digest.exit_code == Some(7))
        );
        assert_eq!(std::fs::read_to_string(&counter).unwrap(), "2");
        let attempted_workspaces = std::fs::read_to_string(&workspaces)
            .unwrap()
            .lines()
            .map(PathBuf::from)
            .collect::<Vec<_>>();
        assert_eq!(attempted_workspaces.len(), 2);
        assert_ne!(attempted_workspaces[0], attempted_workspaces[1]);
        assert!(attempted_workspaces.iter().all(|path| !path.exists()));
        assert_eq!(
            std::fs::read(root.join(WORKSPACE_TARGET)).unwrap(),
            ORIGINAL_BYTES.as_bytes()
        );
        assert!(!root.join("frontier-poison.txt").exists());
        let requests = dispatcher.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].failure_digests().len(), 3);
        assert_eq!(requests[1].failure_digests().len(), 4);
        assert!(requests[1].prompt().contains("tier=frontier"));
        assert_eq!(
            local_run
                .repair_events
                .iter()
                .map(|event| event.transition)
                .collect::<Vec<_>>(),
            vec![
                deepseek_custom::procedure::RepairLadderTransition::Escalated,
                deepseek_custom::procedure::RepairLadderTransition::AttemptStarted,
                deepseek_custom::procedure::RepairLadderTransition::VerifierFailure,
                deepseek_custom::procedure::RepairLadderTransition::AttemptStarted,
                deepseek_custom::procedure::RepairLadderTransition::VerifierFailure,
                deepseek_custom::procedure::RepairLadderTransition::Blocked,
            ]
        );
        let progress_events = std::iter::from_fn(|| progress_rx.try_recv().ok())
            .filter_map(|progress| match progress {
                ProcedureProgress::RepairTransition { event, .. } => Some(event),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(progress_events, local_run.repair_events);

        std::fs::remove_dir_all(root).ok();
        std::fs::remove_dir_all(evidence).ok();
    });
}

// covers: deepseek-custom/bounded-repair-escalation :: Interruption cancels the ladder :: User interrupts during repair
#[test]
fn interrupting_active_frontier_dispatch_kills_the_child_and_stops_the_ladder() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let root = temp_dir("interrupt-dispatch");
        let evidence = temp_dir("interrupt-evidence");
        init_repository(&root);
        write_target(&root, ORIGINAL_BYTES);
        let started = evidence.join("started.txt");
        let completed = evidence.join("completed.txt");
        write_interruptible_frontier_dispatch(&root, &started, &completed);
        let cancelled = Arc::new(AtomicBool::new(false));
        let dispatcher = InterruptibleFrontierDispatcher {
            root: root.clone(),
            started: started.clone(),
            completed: completed.clone(),
            calls: AtomicUsize::new(0),
            cancelled: Arc::clone(&cancelled),
            disposable_paths: Mutex::new(Vec::new()),
        };
        let interrupt = Arc::new(AtomicBool::new(false));
        let mut local_run = workspace_local_run(&root);
        let commands = ["must-not-run".to_string()];
        let runner = FrontierRepairRunner::with_interrupt(root.clone(), Arc::clone(&interrupt));
        let outcome = {
            let run_future = runner.run(&mut local_run, &dispatcher, &commands);
            tokio::pin!(run_future);

            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                tokio::select! {
                    result = &mut run_future => panic!("frontier dispatch ended before interruption: {result:?}"),
                    _ = tokio::time::sleep(Duration::from_millis(10)) => {
                        if started.exists() {
                            break;
                        }
                        assert!(Instant::now() < deadline, "frontier dispatch fixture did not start");
                    }
                }
            }

            interrupt.store(true, Ordering::SeqCst);
            tokio::time::timeout(Duration::from_secs(3), run_future)
                .await
                .expect("interrupted frontier dispatch must finish promptly")
                .unwrap()
        };

        assert_eq!(outcome, FrontierRepairOutcome::Interrupted);
        assert_eq!(local_run.state.disposition(), &AttemptDisposition::Interrupted);
        assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 1);
        assert!(cancelled.load(Ordering::SeqCst));
        assert_eq!(
            std::fs::read(root.join(WORKSPACE_TARGET)).unwrap(),
            ORIGINAL_BYTES.as_bytes()
        );
        assert!(
            dispatcher
                .disposable_paths
                .lock()
                .unwrap()
                .iter()
                .all(|path| !path.exists()),
            "interrupted frontier disposable state leaked"
        );

        tokio::time::sleep(Duration::from_secs(5)).await;
        assert!(!completed.exists(), "interrupted frontier child kept running");
        assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 1);

        std::fs::remove_dir_all(root).ok();
        std::fs::remove_dir_all(evidence).ok();
    });
}
