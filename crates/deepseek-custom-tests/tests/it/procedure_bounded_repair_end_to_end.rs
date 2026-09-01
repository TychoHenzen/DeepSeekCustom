use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use deepseek_custom::config::settings::{BackendConfig, ProcedureSettings, Settings};
use deepseek_custom::procedure::{
    BoundedRepairCoordinator, FrontierPatchDraftError, FrontierRepairDispatch,
    FrontierRepairOutcome, FrontierRepairRequest, FrontierRepairRunner, LocalPatchDraftDispatch,
    LocalPatchDraftError, LocalRepairOutcome, LocalRepairRunner, LocalizationAgreementOutcome,
    LocalizationAgreementResolver, LocalizationAttempt, LocalizationDispatch,
    LocalizationDispatchError, LocalizationEnvelope, LocalizationEscalationTrigger,
    LocalizationSampler, LocalizationTarget, OpenSpecInput, PatchCandidate, PatchEnvelopeError,
    PatchPreview, PatchPreviewId, PatchPreviewStore, ProcedureAttemptDisposition,
    ProcedureReportRepository, ProcedureReviewDisposition, ProcedureRun, ProcedureRunId,
    ProcedureRunMetrics, ProcedureScratchpad, ProcedureStage, ProcedureStageTiming, ProcedureTask,
    ProcedureTerminalDisposition, RepairInputGate, RepairLadderDisposition, RepairLadderGateResult,
    RepairLadderTransition, RepairRequest, RepairTier, RepositoryIndexEntry, RouteDecision,
    RouteOverride, RouteTier, SamplingInputGate, SamplingInputRequest, decode_patch_envelope,
    sha256_json,
};

const CHANGE_ID: &str = "fixture-change";
const TASK_ID: &str = "5.3";
const TARGET: &str = "src/lib.rs";
const UNRELATED: &str = "src/untouched.rs";
const ORIGINAL: &str = "pub const VALUE: &str = \"original\";\n";
const UNRELATED_BYTES: &[u8] = b"pub const UNTOUCHED: bool = true;\n";

fn temp_dir(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "dsc-bounded-repair-e2e-{tag}-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn write_file(root: &Path, relative: &str, contents: impl AsRef<[u8]>) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn init_repository(root: &Path) {
    assert!(
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(root)
            .status()
            .unwrap()
            .success()
    );
}

fn write_openspec_fixture(root: &Path) -> PathBuf {
    let command = if cfg!(windows) {
        root.join("fake-openspec.cmd")
    } else {
        root.join("fake-openspec.sh")
    };
    #[cfg(windows)]
    std::fs::write(&command, "@echo off\r\nexit /b 0\r\n").unwrap();
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(&command, "#!/bin/sh\nexit 0\n").unwrap();
        let mut permissions = std::fs::metadata(&command).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&command, permissions).unwrap();
    }
    write_file(
        root,
        "openspec/changes/fixture-change/proposal.md",
        "## Why\n\nRepair fixture.\n\n## What Changes\n\n- Repair one target.\n",
    );
    write_file(
        root,
        "openspec/changes/fixture-change/tasks.md",
        "- [ ] 5.3 Exercise bounded repair end to end\n",
    );
    write_file(
        root,
        "openspec/changes/fixture-change/specs/deepseek-custom/bounded-repair-escalation/spec.md",
        "## Purpose\n\nFixture.\n\n## ADDED Requirements\n\n### Requirement: Bounded repair\nThe system SHALL use deterministic gates.\n\n#### Scenario: End to end\n- **WHEN** repair runs\n- **THEN** evidence is saved\n",
    );
    command
}

fn candidate(value: &str) -> PatchCandidate {
    let diff = format!(
        "diff --git a/{TARGET} b/{TARGET}\n--- a/{TARGET}\n+++ b/{TARGET}\n@@ -1 +1 @@\n-{ORIGINAL}+pub const VALUE: &str = \"{value}\";\n"
    );
    decode_patch_envelope(
        &serde_json::json!({
            "targets": [TARGET],
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
        .to_string(),
    )
    .unwrap()
}

struct ScriptedLocal {
    replies: Mutex<VecDeque<Result<PatchCandidate, LocalPatchDraftError>>>,
    calls: AtomicUsize,
    workspace_observer: Option<WorkspaceObserver>,
}

impl ScriptedLocal {
    fn new(replies: Vec<Result<PatchCandidate, LocalPatchDraftError>>) -> Self {
        Self {
            replies: Mutex::new(replies.into()),
            calls: AtomicUsize::new(0),
            workspace_observer: None,
        }
    }

    fn observing_workspace(mut self, root: &Path, observations: WorkspaceObservations) -> Self {
        self.workspace_observer = Some(WorkspaceObserver {
            root: root.to_path_buf(),
            observations,
        });
        self
    }
}

#[async_trait]
impl LocalPatchDraftDispatch for ScriptedLocal {
    fn backend_name(&self) -> &str {
        "scripted-local"
    }

    fn model(&self) -> &str {
        "local-model"
    }

    async fn draft(&self, _prompt: String) -> Result<PatchCandidate, LocalPatchDraftError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(observer) = &self.workspace_observer {
            observer.record(format!("local-dispatch-{call}"));
        }
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Err(LocalPatchDraftError::MissingFinalContent))
    }
}

struct ScriptedFrontier {
    replies: Mutex<VecDeque<PatchCandidate>>,
    calls: AtomicUsize,
    requests: Mutex<Vec<FrontierRepairRequest>>,
    workspace_observer: Option<WorkspaceObserver>,
}

impl ScriptedFrontier {
    fn new(replies: Vec<PatchCandidate>) -> Self {
        Self {
            replies: Mutex::new(replies.into()),
            calls: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
            workspace_observer: None,
        }
    }

    fn observing_workspace(mut self, root: &Path, observations: WorkspaceObservations) -> Self {
        self.workspace_observer = Some(WorkspaceObserver {
            root: root.to_path_buf(),
            observations,
        });
        self
    }
}

#[async_trait]
impl FrontierRepairDispatch for ScriptedFrontier {
    fn model(&self, _backend: &str) -> String {
        "frontier-model".to_string()
    }

    async fn draft(
        &self,
        request: &FrontierRepairRequest,
    ) -> Result<PatchCandidate, FrontierPatchDraftError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(observer) = &self.workspace_observer {
            observer.record(format!("frontier-dispatch-{call}"));
        }
        self.requests.lock().unwrap().push(request.clone());
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .ok_or(FrontierPatchDraftError::InvalidOutput(
                PatchEnvelopeError::Structure {
                    reason: "unexpected frontier call".to_string(),
                },
            ))
    }
}

#[derive(Clone)]
struct ScriptedLocalization {
    responses: Arc<Mutex<VecDeque<Result<LocalizationEnvelope, LocalizationDispatchError>>>>,
    calls: Arc<AtomicUsize>,
}

impl ScriptedLocalization {
    fn new(
        responses: impl IntoIterator<Item = Result<LocalizationEnvelope, LocalizationDispatchError>>,
    ) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses.into_iter().collect())),
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }
}

#[async_trait]
impl LocalizationDispatch for ScriptedLocalization {
    fn backend_name(&self) -> &str {
        "scripted-localizer"
    }

    fn model(&self) -> &str {
        "localizer-model"
    }

    async fn dispatch_prompt(
        &self,
        _prompt: String,
        _repository_index: &[RepositoryIndexEntry],
    ) -> Result<LocalizationEnvelope, LocalizationDispatchError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("localization dispatch exceeded the bounded fixture script")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkspaceObservation {
    stage: String,
    target: Vec<u8>,
    unrelated: Vec<u8>,
}

type WorkspaceObservations = Arc<Mutex<Vec<WorkspaceObservation>>>;

struct WorkspaceObserver {
    root: PathBuf,
    observations: WorkspaceObservations,
}

impl WorkspaceObserver {
    fn record(&self, stage: String) {
        self.observations
            .lock()
            .unwrap()
            .push(WorkspaceObservation {
                stage,
                target: std::fs::read(self.root.join(TARGET)).unwrap(),
                unrelated: std::fs::read(self.root.join(UNRELATED)).unwrap(),
            });
    }
}

struct Fixture {
    root: PathBuf,
    request: RepairRequest,
    reports: ProcedureReportRepository,
    openspec: PathBuf,
    verifier: String,
    verifier_observations: PathBuf,
}

impl Fixture {
    fn new(tag: &str, interrupt_marker: bool) -> Self {
        let root = temp_dir(tag);
        init_repository(&root);
        write_file(&root, TARGET, ORIGINAL);
        write_file(&root, UNRELATED, UNRELATED_BYTES);
        let openspec = write_openspec_fixture(&root);
        let validated = OpenSpecInput::with_command(&root, openspec.display().to_string())
            .validate_and_select_task(CHANGE_ID, TASK_ID)
            .unwrap();
        let report = ProcedureRun {
            id: ProcedureRunId::new(),
            change_id: CHANGE_ID.to_string(),
            selected_task: ProcedureTask {
                id: TASK_ID.to_string(),
                text: validated.contract.task.text.clone(),
                covers: validated.contract.task.covers.clone(),
            },
            spec_fingerprint: Some(sha256_json(&validated.contract).unwrap()),
            repository_fingerprint: Some("sha256:fixture".to_string()),
            validation: Some(validated.validation),
            scratchpad: ProcedureScratchpad {
                goals: vec!["Repair the fixture.".to_string()],
                files: vec![TARGET.to_string()],
                changes: Vec::new(),
                last_error: None,
            },
            stage: ProcedureStage::Finished,
            attempts: vec![LocalizationAttempt {
                number: 1,
                backend: "fixture-localizer".to_string(),
                model: "fixture-model".to_string(),
                disposition: ProcedureAttemptDisposition::Accepted,
                targets: vec![LocalizationTarget {
                    path: TARGET.to_string(),
                    symbol: None,
                    evidence: "The fixture target owns the change.".to_string(),
                }],
                validation_error: None,
            }],
            review_disposition: ProcedureReviewDisposition::Approved,
            terminal_disposition: Some(ProcedureTerminalDisposition::AwaitingReview),
        };
        let reports = ProcedureReportRepository::for_project(&root);
        reports.save(&report).unwrap();
        let preview = PatchPreview {
            id: PatchPreviewId::new(),
            localization_run_id: report.id,
            change_id: CHANGE_ID.to_string(),
            task_id: TASK_ID.to_string(),
            route: RouteDecision {
                automatic_tier: RouteTier::Local,
                effective_tier: RouteTier::Local,
                signals: Vec::new(),
                selected_override: RouteOverride::Automatic,
                overridden: false,
            },
            backend: "scripted-local".to_string(),
            model: "local-model".to_string(),
            targets: vec![TARGET.to_string()],
            rationale: "Trusted patch state.".to_string(),
            unified_diff: candidate("preview").envelope().unified_diff.clone(),
        };
        PatchPreviewStore::for_project(&root)
            .save(&preview)
            .unwrap();
        let verifier_observations = root.join("verifier-workspace-observations.txt");
        let verifier = write_verifier(&root, interrupt_marker, &verifier_observations);
        Self {
            root,
            request: RepairRequest {
                localization_run_id: report.id,
                preview_id: preview.id,
                change_id: CHANGE_ID.to_string(),
                task_id: TASK_ID.to_string(),
            },
            reports,
            openspec,
            verifier,
            verifier_observations,
        }
    }

    fn assert_unchanged(&self) {
        assert_eq!(
            std::fs::read(self.root.join(TARGET)).unwrap(),
            ORIGINAL.as_bytes()
        );
        assert_eq!(
            std::fs::read(self.root.join(UNRELATED)).unwrap(),
            UNRELATED_BYTES
        );
    }

    fn assert_saved(&self, run: &deepseek_custom::procedure::BoundedRepairRun) {
        let stored = self
            .reports
            .load_with_fingerprints(&self.request.localization_run_id)
            .unwrap();
        assert_eq!(stored.repair_events, run.persisted_events);
        assert_eq!(run.persisted_events, run.local.repair_events);
    }

    fn assert_verifier_observed_original_workspace(&self, expected_runs: usize) {
        let observations = std::fs::read_to_string(&self.verifier_observations).unwrap();
        let lines = observations.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), expected_runs);
        assert!(
            lines
                .iter()
                .all(|line| { *line == "verifier|target_original=true|unrelated_original=true" })
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).ok();
    }
}

fn write_verifier(root: &Path, marker_only: bool, observations: &Path) -> String {
    #[cfg(windows)]
    {
        let command = root.join("repair-verifier.ps1");
        let marker = root
            .join("verifier-ran.txt")
            .display()
            .to_string()
            .replace('\'', "''");
        let target = root.join(TARGET).display().to_string().replace('\'', "''");
        let unrelated = root
            .join(UNRELATED)
            .display()
            .to_string()
            .replace('\'', "''");
        let observations = observations.display().to_string().replace('\'', "''");
        let target_hex = hex(ORIGINAL.as_bytes());
        let unrelated_hex = hex(UNRELATED_BYTES);
        let body = if marker_only {
            format!("Set-Content -LiteralPath '{marker}' -Value ran\nexit 0\n")
        } else {
            format!(
                "$targetHex = [BitConverter]::ToString([IO.File]::ReadAllBytes('{target}')).Replace('-', '')\n\
                 $unrelatedHex = [BitConverter]::ToString([IO.File]::ReadAllBytes('{unrelated}')).Replace('-', '')\n\
                 if ($targetHex -ne '{target_hex}') {{ Write-Error 'real target changed before promotion'; exit 91 }}\n\
                 if ($unrelatedHex -ne '{unrelated_hex}') {{ Write-Error 'unrelated real-workspace bytes changed'; exit 92 }}\n\
                 Add-Content -LiteralPath '{observations}' -Value 'verifier|target_original=true|unrelated_original=true'\n\
                 $content = Get-Content -Raw -LiteralPath 'src/lib.rs'\n\
                 if ($content.Contains('pass')) {{ exit 0 }}\n\
                 Write-Error 'deterministic repair failure'\n\
                 exit 7\n"
            )
        };
        std::fs::write(&command, body).unwrap();
        format!(
            "powershell.exe -NoProfile -ExecutionPolicy Bypass -File \"{}\"",
            command.display()
        )
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        let command = root.join("repair-verifier.sh");
        let marker = root
            .join("verifier-ran.txt")
            .display()
            .to_string()
            .replace('\'', "'\\''");
        let target = root
            .join(TARGET)
            .display()
            .to_string()
            .replace('\'', "'\\''");
        let unrelated = root
            .join(UNRELATED)
            .display()
            .to_string()
            .replace('\'', "'\\''");
        let observations = observations.display().to_string().replace('\'', "'\\''");
        let target_hex = hex(ORIGINAL.as_bytes()).to_ascii_lowercase();
        let unrelated_hex = hex(UNRELATED_BYTES).to_ascii_lowercase();
        let body = if marker_only {
            format!("#!/bin/sh\nprintf ran > '{marker}'\nexit 0\n")
        } else {
            format!(
                "#!/bin/sh\n\
                 target_hex=$(od -An -tx1 '{target}' | tr -d ' \\n')\n\
                 unrelated_hex=$(od -An -tx1 '{unrelated}' | tr -d ' \\n')\n\
                 [ \"$target_hex\" = \"{target_hex}\" ] || {{ echo 'real target changed before promotion' >&2; exit 91; }}\n\
                 [ \"$unrelated_hex\" = \"{unrelated_hex}\" ] || {{ echo 'unrelated real-workspace bytes changed' >&2; exit 92; }}\n\
                 printf '%s\\n' 'verifier|target_original=true|unrelated_original=true' >> '{observations}'\n\
                 grep -q pass src/lib.rs && exit 0\n\
                 echo 'deterministic repair failure' >&2\n\
                 exit 7\n"
            )
        };
        std::fs::write(&command, body).unwrap();
        let mut permissions = std::fs::metadata(&command).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&command, permissions).unwrap();
        format!("\"{}\"", command.display())
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02X}")).collect()
}

fn policy(
    frontier_attempts: u8,
) -> deepseek_custom::config::settings::ValidatedProcedureRepairPolicy {
    let frontier_backend = (frontier_attempts > 0).then(|| "scripted-frontier".to_string());
    let backends = frontier_backend.as_ref().map(|name| {
        HashMap::from([(
            name.clone(),
            BackendConfig::CodexCli {
                model: "frontier-model".to_string(),
                sandbox: Some("workspace-write".to_string()),
                env: None,
                models: None,
            },
        )])
    });
    Settings {
        procedure: Some(ProcedureSettings {
            structural_retries: 1,
            local_verifier_attempts: 3,
            frontier_patch_backend: frontier_backend,
            frontier_attempts,
            ..ProcedureSettings::default()
        }),
        backends,
        ..Settings::default()
    }
    .validated_procedure_repair_policy()
    .unwrap()
}

fn run(
    fixture: &Fixture,
    interrupt: Arc<AtomicBool>,
    local: &ScriptedLocal,
    frontier: Option<&ScriptedFrontier>,
    frontier_attempts: u8,
) -> deepseek_custom::procedure::BoundedRepairRun {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let gate = RepairInputGate::new(
        OpenSpecInput::with_command(&fixture.root, fixture.openspec.display().to_string()),
        fixture.root.clone(),
        fixture.reports.clone(),
    );
    let local_runner = LocalRepairRunner::new(gate, fixture.root.clone(), interrupt.clone());
    let frontier_runner = FrontierRepairRunner::with_interrupt(fixture.root.clone(), interrupt);
    runtime
        .block_on(
            BoundedRepairCoordinator::new(&local_runner, &frontier_runner, &fixture.reports).run(
                &fixture.request,
                policy(frontier_attempts),
                local,
                frontier.map(|value| value as &dyn FrontierRepairDispatch),
                std::slice::from_ref(&fixture.verifier),
            ),
        )
        .unwrap()
}

fn approved_sampling_input(
    fixture: &Fixture,
) -> deepseek_custom::procedure::ValidatedSamplingInput {
    SamplingInputGate::new(
        OpenSpecInput::with_command(&fixture.root, fixture.openspec.display().to_string()),
        fixture.root.clone(),
        fixture.reports.clone(),
    )
    .load(&SamplingInputRequest {
        baseline_localization_run_id: fixture.request.localization_run_id,
        change_id: CHANGE_ID.to_string(),
        task_id: TASK_ID.to_string(),
    })
    .unwrap()
}

fn localization_envelope(path: &str, symbol: Option<&str>) -> LocalizationEnvelope {
    LocalizationEnvelope {
        targets: vec![LocalizationTarget {
            path: path.to_string(),
            symbol: symbol.map(str::to_string),
            evidence: "bounded end-to-end fixture target".to_string(),
        }],
    }
}

fn localization_index() -> Vec<RepositoryIndexEntry> {
    vec![
        RepositoryIndexEntry {
            path: TARGET.to_string(),
            symbols: vec!["VALUE".to_string()],
        },
        RepositoryIndexEntry {
            path: UNRELATED.to_string(),
            symbols: Vec::new(),
        },
    ]
}

#[test]
fn parser_recovery_persists_the_complete_success_sequence() {
    let fixture = Fixture::new("parser-recovery", false);
    let local = ScriptedLocal::new(vec![
        Err(LocalPatchDraftError::MissingFinalContent),
        Ok(candidate("pass-parser")),
    ]);
    let run = run(&fixture, Arc::new(AtomicBool::new(false)), &local, None, 0);

    assert!(matches!(
        run.local.outcome,
        LocalRepairOutcome::Promoted { .. }
    ));
    assert_eq!(local.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        run.local
            .repair_events
            .iter()
            .map(|event| event.transition)
            .collect::<Vec<_>>(),
        [
            RepairLadderTransition::AttemptStarted,
            RepairLadderTransition::StructuralRetry,
            RepairLadderTransition::Promoted,
        ]
    );
    assert_eq!(
        run.local.repair_events.last().unwrap().disposition,
        RepairLadderDisposition::Promoted
    );
    fixture.assert_saved(&run);
    assert_eq!(
        std::fs::read_to_string(fixture.root.join(TARGET)).unwrap(),
        "pub const VALUE: &str = \"pass-parser\";\n"
    );
    assert_eq!(
        std::fs::read(fixture.root.join(UNRELATED)).unwrap(),
        UNRELATED_BYTES
    );
}

#[test]
fn local_recovery_persists_fail_retry_promote_sequence() {
    let fixture = Fixture::new("local-recovery", false);
    let local = ScriptedLocal::new(vec![
        Ok(candidate("fail-local")),
        Ok(candidate("pass-local")),
    ]);
    let run = run(&fixture, Arc::new(AtomicBool::new(false)), &local, None, 0);

    assert!(matches!(
        run.local.outcome,
        LocalRepairOutcome::Promoted {
            attempt_number: 2,
            ..
        }
    ));
    assert_eq!(local.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        run.local.repair_events.last().unwrap().disposition,
        RepairLadderDisposition::Promoted
    );
    fixture.assert_saved(&run);
    assert_eq!(
        std::fs::read_to_string(fixture.root.join(TARGET)).unwrap(),
        "pub const VALUE: &str = \"pass-local\";\n"
    );
    assert_eq!(
        std::fs::read(fixture.root.join(UNRELATED)).unwrap(),
        UNRELATED_BYTES
    );
}

#[test]
fn local_exhaustion_persists_blocked_sequence_without_workspace_changes() {
    let fixture = Fixture::new("local-exhaustion", false);
    let local = ScriptedLocal::new(vec![
        Ok(candidate("fail-one")),
        Ok(candidate("fail-two")),
        Ok(candidate("fail-three")),
    ]);
    let run = run(&fixture, Arc::new(AtomicBool::new(false)), &local, None, 0);

    assert_eq!(run.local.outcome, LocalRepairOutcome::Blocked);
    assert_eq!(local.calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        run.local.repair_events.last().unwrap().disposition,
        RepairLadderDisposition::Blocked
    );
    fixture.assert_saved(&run);
    fixture.assert_unchanged();
}

#[test]
fn frontier_recovery_runs_both_tiers_and_persists_promoted_sequence() {
    let fixture = Fixture::new("frontier-recovery", false);
    let workspace_observations = Arc::new(Mutex::new(Vec::new()));
    let local = ScriptedLocal::new(vec![
        Ok(candidate("fail-one")),
        Ok(candidate("fail-two")),
        Ok(candidate("fail-three")),
    ])
    .observing_workspace(&fixture.root, Arc::clone(&workspace_observations));
    let frontier = ScriptedFrontier::new(vec![candidate("pass-frontier")])
        .observing_workspace(&fixture.root, Arc::clone(&workspace_observations));
    let run = run(
        &fixture,
        Arc::new(AtomicBool::new(false)),
        &local,
        Some(&frontier),
        2,
    );

    assert!(matches!(
        run.frontier,
        Some(FrontierRepairOutcome::Promoted {
            attempt_number: 1,
            ..
        })
    ));
    assert_eq!(local.calls.load(Ordering::SeqCst), 3);
    assert_eq!(frontier.calls.load(Ordering::SeqCst), 1);
    let frontier_requests = frontier.requests.lock().unwrap();
    assert_eq!(frontier_requests.len(), 1);
    let frontier_request = &frontier_requests[0];
    assert_eq!(frontier_request.backend(), "scripted-frontier");
    assert_eq!(frontier_request.change_id(), CHANGE_ID);
    assert_eq!(frontier_request.task().id, TASK_ID);
    assert_eq!(frontier_request.targets(), [TARGET]);
    assert_eq!(frontier_request.failure_digests().len(), 3);
    assert_eq!(
        frontier_request
            .failure_digests()
            .iter()
            .map(|failure| (failure.attempt_number, failure.tier, failure.exit_code))
            .collect::<Vec<_>>(),
        [
            (1, RepairTier::Local, Some(7)),
            (2, RepairTier::Local, Some(7)),
            (3, RepairTier::Local, Some(7)),
        ]
    );
    assert!(
        frontier_request
            .failure_digests()
            .iter()
            .all(|failure| failure.diagnostic.contains("deterministic repair failure"))
    );
    assert_eq!(
        run.persisted_events
            .iter()
            .map(|event| {
                (
                    event.transition,
                    event.attempt_number,
                    event.tier,
                    event.backend.as_str(),
                    event.model.as_str(),
                    event.gate_result,
                    event.disposition,
                )
            })
            .collect::<Vec<_>>(),
        [
            (
                RepairLadderTransition::AttemptStarted,
                1,
                RepairTier::Local,
                "scripted-local",
                "local-model",
                RepairLadderGateResult::NotRun,
                RepairLadderDisposition::CandidateActive,
            ),
            (
                RepairLadderTransition::VerifierFailure,
                1,
                RepairTier::Local,
                "scripted-local",
                "local-model",
                RepairLadderGateResult::VerifierFailed,
                RepairLadderDisposition::Ready,
            ),
            (
                RepairLadderTransition::AttemptStarted,
                2,
                RepairTier::Local,
                "scripted-local",
                "local-model",
                RepairLadderGateResult::NotRun,
                RepairLadderDisposition::CandidateActive,
            ),
            (
                RepairLadderTransition::VerifierFailure,
                2,
                RepairTier::Local,
                "scripted-local",
                "local-model",
                RepairLadderGateResult::VerifierFailed,
                RepairLadderDisposition::Ready,
            ),
            (
                RepairLadderTransition::AttemptStarted,
                3,
                RepairTier::Local,
                "scripted-local",
                "local-model",
                RepairLadderGateResult::NotRun,
                RepairLadderDisposition::CandidateActive,
            ),
            (
                RepairLadderTransition::VerifierFailure,
                3,
                RepairTier::Local,
                "scripted-local",
                "local-model",
                RepairLadderGateResult::VerifierFailed,
                RepairLadderDisposition::LocalExhausted,
            ),
            (
                RepairLadderTransition::Escalated,
                1,
                RepairTier::Frontier,
                "scripted-frontier",
                "frontier-model",
                RepairLadderGateResult::NotRun,
                RepairLadderDisposition::CandidateActive,
            ),
            (
                RepairLadderTransition::AttemptStarted,
                1,
                RepairTier::Frontier,
                "scripted-frontier",
                "frontier-model",
                RepairLadderGateResult::NotRun,
                RepairLadderDisposition::CandidateActive,
            ),
            (
                RepairLadderTransition::Promoted,
                1,
                RepairTier::Frontier,
                "scripted-frontier",
                "frontier-model",
                RepairLadderGateResult::VerifierPassed,
                RepairLadderDisposition::Promoted,
            ),
        ]
    );
    assert_eq!(
        run.local.repair_events.last().unwrap().disposition,
        RepairLadderDisposition::Promoted
    );
    fixture.assert_saved(&run);
    let observations = workspace_observations.lock().unwrap();
    assert_eq!(
        observations
            .iter()
            .map(|observation| observation.stage.as_str())
            .collect::<Vec<_>>(),
        [
            "local-dispatch-1",
            "local-dispatch-2",
            "local-dispatch-3",
            "frontier-dispatch-1",
        ]
    );
    assert!(observations.iter().all(|observation| {
        observation.target == ORIGINAL.as_bytes() && observation.unrelated == UNRELATED_BYTES
    }));
    fixture.assert_verifier_observed_original_workspace(4);
    assert_eq!(
        std::fs::read_to_string(fixture.root.join(TARGET)).unwrap(),
        "pub const VALUE: &str = \"pass-frontier\";\n"
    );
    assert_eq!(
        std::fs::read(fixture.root.join(UNRELATED)).unwrap(),
        UNRELATED_BYTES
    );
}

#[test]
fn frontier_exhaustion_persists_blocked_sequence_without_workspace_changes() {
    let fixture = Fixture::new("frontier-exhaustion", false);
    let local = ScriptedLocal::new(vec![
        Ok(candidate("fail-one")),
        Ok(candidate("fail-two")),
        Ok(candidate("fail-three")),
    ]);
    let frontier = ScriptedFrontier::new(vec![candidate("fail-four"), candidate("fail-five")]);
    let run = run(
        &fixture,
        Arc::new(AtomicBool::new(false)),
        &local,
        Some(&frontier),
        2,
    );

    assert!(matches!(
        run.frontier,
        Some(FrontierRepairOutcome::Blocked { attempts: 2, .. })
    ));
    assert_eq!(frontier.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        run.local.repair_events.last().unwrap().disposition,
        RepairLadderDisposition::Blocked
    );
    fixture.assert_saved(&run);
    fixture.assert_unchanged();
}

#[test]
fn interruption_persists_terminal_sequence_without_dispatch_or_workspace_changes() {
    let fixture = Fixture::new("interruption", true);
    let local = ScriptedLocal::new(vec![Ok(candidate("pass-must-not-dispatch"))]);
    let interrupt = Arc::new(AtomicBool::new(true));
    let run = run(&fixture, interrupt, &local, None, 0);

    assert_eq!(run.local.outcome, LocalRepairOutcome::Interrupted);
    assert_eq!(local.calls.load(Ordering::SeqCst), 0);
    assert!(!fixture.root.join("verifier-ran.txt").exists());
    assert_eq!(
        run.local.repair_events.last().unwrap().disposition,
        RepairLadderDisposition::Interrupted
    );
    fixture.assert_saved(&run);
    fixture.assert_unchanged();
}

// covers: deepseek-custom/routing-sampling-and-metrics :: The completed procedure remains bounded end to end :: End-to-end escalation
#[test]
fn disagreement_and_local_exhaustion_use_bounded_frontier_promotion_with_durable_evidence() {
    let fixture = Fixture::new("sampling-disagreement-to-frontier", false);
    let input = approved_sampling_input(&fixture);
    let interrupt = Arc::new(AtomicBool::new(false));
    let local_localization = ScriptedLocalization::new([
        Ok(localization_envelope(TARGET, None)),
        Ok(localization_envelope(UNRELATED, None)),
        Ok(localization_envelope(TARGET, Some("VALUE"))),
    ]);
    let frontier_localization =
        ScriptedLocalization::new([Ok(localization_envelope(TARGET, None))]);
    let settings = Settings::default()
        .validated_procedure_sampling_settings()
        .unwrap();
    assert_eq!(settings.localization_sample_count(), 3);
    assert_eq!(settings.localization_agreement_quorum(), 2);
    let resolver = LocalizationAgreementResolver::new(
        LocalizationSampler::new(local_localization.clone(), settings, Arc::clone(&interrupt)),
        frontier_localization.clone(),
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let localization = runtime
        .block_on(resolver.resolve(&input, &localization_index()))
        .unwrap();

    assert_eq!(local_localization.calls.load(Ordering::SeqCst), 3);
    assert_eq!(frontier_localization.calls.load(Ordering::SeqCst), 1);
    assert!(matches!(
        localization.outcome,
        LocalizationAgreementOutcome::Frontier {
            escalation_trigger: LocalizationEscalationTrigger::LocalDisagreement,
            ref targets,
            ..
        } if targets == &[LocalizationTarget {
            path: TARGET.to_string(),
            symbol: None,
            evidence: "bounded end-to-end fixture target".to_string(),
        }]
    ));

    let local_repair = ScriptedLocal::new(vec![
        Ok(candidate("fail-local-one")),
        Ok(candidate("fail-local-two")),
        Ok(candidate("fail-local-three")),
    ]);
    let frontier_repair = ScriptedFrontier::new(vec![candidate("pass-frontier")]);
    let repair = run(
        &fixture,
        interrupt,
        &local_repair,
        Some(&frontier_repair),
        2,
    );

    assert_eq!(local_repair.calls.load(Ordering::SeqCst), 3);
    assert_eq!(frontier_repair.calls.load(Ordering::SeqCst), 1);
    assert!(matches!(
        repair.frontier,
        Some(FrontierRepairOutcome::Promoted {
            attempt_number: 1,
            ..
        })
    ));
    assert_eq!(
        repair.local.repair_events.last().unwrap().disposition,
        RepairLadderDisposition::Promoted
    );
    assert!(repair.local.repair_events.iter().any(|event| {
        event.transition == RepairLadderTransition::VerifierFailure
            && event.tier == RepairTier::Local
            && event.attempt_number == 3
            && event.disposition == RepairLadderDisposition::LocalExhausted
    }));
    assert!(repair.local.repair_events.iter().any(|event| {
        event.transition == RepairLadderTransition::Promoted
            && event.tier == RepairTier::Frontier
            && event.attempt_number == 1
    }));

    let mut metrics = ProcedureRunMetrics::from_terminal_run(&input.report).unwrap();
    metrics.stage_timings.extend([
        ProcedureStageTiming {
            stage: "agreement_sampling".to_string(),
            duration_ms: 1,
        },
        ProcedureStageTiming {
            stage: "frontier_localization".to_string(),
            duration_ms: 1,
        },
        ProcedureStageTiming {
            stage: "bounded_repair".to_string(),
            duration_ms: 1,
        },
        ProcedureStageTiming {
            stage: "frontier_promotion".to_string(),
            duration_ms: 1,
        },
    ]);
    metrics.route.selected_tier = Some(RouteTier::Frontier);
    metrics.route.local_mechanical_success = Some(false);
    metrics.route.escalation_triggers = vec!["local_disagreement".to_string()];
    fixture
        .reports
        .replace_metrics(&input.report.id, &metrics)
        .unwrap();
    let stored = fixture
        .reports
        .load_with_fingerprints(&input.report.id)
        .unwrap();
    assert_eq!(stored.repair_events, repair.persisted_events);
    let metrics = stored.metrics.unwrap();
    assert_eq!(metrics.route.selected_tier, Some(RouteTier::Frontier));
    assert_eq!(metrics.route.local_mechanical_success, Some(false));
    assert_eq!(metrics.route.escalation_triggers, ["local_disagreement"]);
    assert_eq!(
        metrics
            .stage_timings
            .iter()
            .map(|timing| timing.stage.as_str())
            .collect::<Vec<_>>(),
        [
            "agreement_sampling",
            "frontier_localization",
            "bounded_repair",
            "frontier_promotion",
        ]
    );
    assert_eq!(
        std::fs::read_to_string(fixture.root.join(TARGET)).unwrap(),
        "pub const VALUE: &str = \"pass-frontier\";\n"
    );
    assert_eq!(
        std::fs::read(fixture.root.join(UNRELATED)).unwrap(),
        UNRELATED_BYTES
    );
}

#[test]
fn local_and_frontier_budget_exhaustion_persist_a_blocked_report_without_extra_dispatch() {
    let fixture = Fixture::new("sampling-local-and-frontier-exhaustion", false);
    let local_repair = ScriptedLocal::new(vec![
        Ok(candidate("fail-local-one")),
        Ok(candidate("fail-local-two")),
        Ok(candidate("fail-local-three")),
    ]);
    let frontier_repair = ScriptedFrontier::new(vec![
        candidate("fail-frontier-one"),
        candidate("fail-frontier-two"),
    ]);
    let repair = run(
        &fixture,
        Arc::new(AtomicBool::new(false)),
        &local_repair,
        Some(&frontier_repair),
        2,
    );

    assert_eq!(local_repair.calls.load(Ordering::SeqCst), 3);
    assert_eq!(frontier_repair.calls.load(Ordering::SeqCst), 2);
    assert!(matches!(
        repair.frontier,
        Some(FrontierRepairOutcome::Blocked { attempts: 2, .. })
    ));
    assert_eq!(
        repair.local.repair_events.last().unwrap().disposition,
        RepairLadderDisposition::Blocked
    );
    assert!(repair.local.repair_events.iter().any(|event| {
        event.transition == RepairLadderTransition::Blocked
            && event.tier == RepairTier::Frontier
            && event.attempt_number == 2
    }));
    assert_eq!(
        fixture
            .reports
            .load_with_fingerprints(&fixture.request.localization_run_id)
            .unwrap()
            .repair_events,
        repair.persisted_events
    );
    fixture.assert_unchanged();
}
