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
    LocalPatchDraftError, LocalRepairOutcome, LocalRepairRunner, LocalizationAttempt,
    LocalizationTarget, OpenSpecInput, PatchCandidate, PatchEnvelopeError, PatchPreview,
    PatchPreviewId, PatchPreviewStore, ProcedureAttemptDisposition, ProcedureReportStore,
    ProcedureReviewDisposition, ProcedureRun, ProcedureRunId, ProcedureScratchpad, ProcedureStage,
    ProcedureTask, ProcedureTerminalDisposition, RepairInputGate, RepairLadderDisposition,
    RepairLadderTransition, RepairRequest, RouteDecision, RouteOverride, RouteTier,
    decode_patch_envelope, sha256_json,
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
}

impl ScriptedLocal {
    fn new(replies: Vec<Result<PatchCandidate, LocalPatchDraftError>>) -> Self {
        Self {
            replies: Mutex::new(replies.into()),
            calls: AtomicUsize::new(0),
        }
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
        self.calls.fetch_add(1, Ordering::SeqCst);
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
}

impl ScriptedFrontier {
    fn new(replies: Vec<PatchCandidate>) -> Self {
        Self {
            replies: Mutex::new(replies.into()),
            calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl FrontierRepairDispatch for ScriptedFrontier {
    fn model(&self, _backend: &str) -> String {
        "frontier-model".to_string()
    }

    async fn draft(
        &self,
        _request: &FrontierRepairRequest,
    ) -> Result<PatchCandidate, FrontierPatchDraftError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
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

struct Fixture {
    root: PathBuf,
    request: RepairRequest,
    reports: ProcedureReportStore,
    openspec: PathBuf,
    verifier: String,
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
        let reports = ProcedureReportStore::for_project(&root);
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
        let verifier = write_verifier(&root, interrupt_marker);
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
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).ok();
    }
}

fn write_verifier(root: &Path, marker_only: bool) -> String {
    #[cfg(windows)]
    {
        let command = root.join("repair-verifier.ps1");
        let marker = root
            .join("verifier-ran.txt")
            .display()
            .to_string()
            .replace('\'', "''");
        let body = if marker_only {
            format!("Set-Content -LiteralPath '{marker}' -Value ran\nexit 0\n")
        } else {
            "$content = Get-Content -Raw -LiteralPath 'src/lib.rs'\nif ($content.Contains('pass')) { exit 0 }\nWrite-Error 'deterministic repair failure'\nexit 7\n".to_string()
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
        let body = if marker_only {
            format!("#!/bin/sh\nprintf ran > '{marker}'\nexit 0\n")
        } else {
            "#!/bin/sh\ngrep -q pass src/lib.rs && exit 0\necho 'deterministic repair failure' >&2\nexit 7\n".to_string()
        };
        std::fs::write(&command, body).unwrap();
        let mut permissions = std::fs::metadata(&command).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&command, permissions).unwrap();
        format!("\"{}\"", command.display())
    }
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
    let local = ScriptedLocal::new(vec![
        Ok(candidate("fail-one")),
        Ok(candidate("fail-two")),
        Ok(candidate("fail-three")),
    ]);
    let frontier = ScriptedFrontier::new(vec![candidate("pass-frontier")]);
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
    assert_eq!(
        run.local.repair_events.last().unwrap().disposition,
        RepairLadderDisposition::Promoted
    );
    fixture.assert_saved(&run);
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
