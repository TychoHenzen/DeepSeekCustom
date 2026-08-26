use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use deepseek_custom::config::settings::{ProcedureSettings, Settings};
use deepseek_custom::procedure::{
    AttemptDisposition, LocalPatchDraftDispatch, LocalPatchDraftError, LocalRepairOutcome,
    LocalRepairRunner, LocalizationAttempt, LocalizationTarget, OpenSpecInput, PatchCandidate,
    PatchPreview, PatchPreviewId, PatchPreviewStore, ProcedureAttemptDisposition,
    ProcedureReportStore, ProcedureReviewDisposition, ProcedureRun, ProcedureRunId,
    ProcedureScratchpad, ProcedureStage, ProcedureTask, ProcedureTerminalDisposition,
    RepairInputGate, RepairRequest, RouteDecision, RouteOverride, RouteTier, decode_patch_envelope,
    sha256_json,
};

const CHANGE_ID: &str = "fixture-change";
const TASK_ID: &str = "3.3";
const TARGET: &str = "src/lib.rs";
const ORIGINAL: &str = "pub const VALUE: &str = \"original\";\n";

fn temp_dir() -> PathBuf {
    let root = std::env::temp_dir().join(format!("dsc-local-repair-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn write_file(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn init_repository(root: &Path) {
    let status = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(root)
        .status()
        .unwrap();
    assert!(status.success(), "git init failed with {status}");
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
        "- [ ] 3.3 Promote a passing local repair\n  <!-- covers: deepseek-custom/bounded-repair-escalation :: Verifier failures have a bounded local repair budget :: Local repair passes within budget -->\n",
    );
    write_file(
        root,
        "openspec/changes/fixture-change/specs/deepseek-custom/bounded-repair-escalation/spec.md",
        "## Purpose\n\nFixture.\n\n## ADDED Requirements\n\n### Requirement: Verifier failures have a bounded local repair budget\nThe system SHALL allow at most three local verifier-driven patch attempts by default.\n\n#### Scenario: Local repair passes within budget\n- **WHEN** a local repair candidate passes every gate on attempt three or earlier\n- **THEN** it is promoted through the verification gate and no frontier request is made\n",
    );
    command
}

fn write_verifier(root: &Path) -> PathBuf {
    let command = if cfg!(windows) {
        root.join("passing-verifier.cmd")
    } else {
        root.join("passing-verifier.sh")
    };
    #[cfg(windows)]
    std::fs::write(
        &command,
        "@echo off\r\nset /p candidate=<src\\lib.rs\r\nif not \"%candidate:passing=%\"==\"%candidate%\" exit /b 0\r\necho candidate must contain passing 1>&2\r\nexit /b 7\r\n",
    )
    .unwrap();
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(
            &command,
            "#!/bin/sh\nif ! grep -q passing src/lib.rs; then echo 'candidate must contain passing' >&2; exit 7; fi\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&command).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&command, permissions).unwrap();
    }
    command
}

fn candidate(value: &str) -> PatchCandidate {
    let diff = format!(
        "diff --git a/{TARGET} b/{TARGET}\n--- a/{TARGET}\n+++ b/{TARGET}\n@@ -1 +1 @@\n-{original}+pub const VALUE: &str = \"{value}\";\n",
        original = ORIGINAL,
    );
    let json = serde_json::json!({
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
    });
    decode_patch_envelope(&json.to_string()).unwrap()
}

struct ScriptedLocalDispatcher {
    candidates: Mutex<VecDeque<PatchCandidate>>,
    prompts: Mutex<Vec<String>>,
    calls: AtomicUsize,
    real_target: PathBuf,
    real_bytes_seen_at_dispatch: Mutex<Vec<Vec<u8>>>,
}

impl ScriptedLocalDispatcher {
    fn new(root: &Path, candidates: Vec<PatchCandidate>) -> Self {
        Self {
            candidates: Mutex::new(candidates.into()),
            prompts: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
            real_target: root.join(TARGET),
            real_bytes_seen_at_dispatch: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl LocalPatchDraftDispatch for ScriptedLocalDispatcher {
    fn backend_name(&self) -> &str {
        "scripted-local"
    }

    fn model(&self) -> &str {
        "scripted-model"
    }

    async fn draft(&self, prompt: String) -> Result<PatchCandidate, LocalPatchDraftError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.prompts.lock().unwrap().push(prompt);
        self.real_bytes_seen_at_dispatch
            .lock()
            .unwrap()
            .push(std::fs::read(&self.real_target).unwrap());
        self.candidates
            .lock()
            .unwrap()
            .pop_front()
            .ok_or(LocalPatchDraftError::MissingFinalContent)
    }
}

fn save_trusted_input(root: &Path, openspec_command: &Path) -> (ProcedureRun, PatchPreview) {
    let validated = OpenSpecInput::with_command(root, openspec_command.display().to_string())
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
    ProcedureReportStore::for_project(root)
        .save(&report)
        .unwrap();

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
        model: "scripted-model".to_string(),
        targets: vec![TARGET.to_string()],
        rationale: "Initial preview establishes the trusted patch state.".to_string(),
        unified_diff: candidate("preview").envelope().unified_diff.clone(),
    };
    PatchPreviewStore::for_project(root).save(&preview).unwrap();
    (report, preview)
}

fn policy() -> deepseek_custom::config::settings::ValidatedProcedureRepairPolicy {
    Settings {
        procedure: Some(ProcedureSettings {
            structural_retries: 1,
            local_verifier_attempts: 3,
            frontier_attempts: 0,
            ..ProcedureSettings::default()
        }),
        ..Settings::default()
    }
    .validated_procedure_repair_policy()
    .unwrap()
}

// covers: deepseek-custom/bounded-repair-escalation :: Verifier failures have a bounded local repair budget :: Local repair passes within budget
#[test]
fn local_attempt_three_promotes_after_real_gates_without_frontier_dispatch() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let root = temp_dir();
        init_repository(&root);
        write_file(&root, TARGET, ORIGINAL);
        let openspec_command = write_openspec_fixture(&root);
        let verifier = write_verifier(&root);
        let (report, preview) = save_trusted_input(&root, &openspec_command);
        let dispatcher = ScriptedLocalDispatcher::new(
            &root,
            vec![
                candidate("first"),
                candidate("second"),
                candidate("passing"),
            ],
        );
        let frontier_calls = AtomicUsize::new(0);
        let gate = RepairInputGate::new(
            OpenSpecInput::with_command(&root, openspec_command.display().to_string()),
            root.clone(),
            ProcedureReportStore::for_project(&root),
        );
        let runner = LocalRepairRunner::new(
            gate,
            root.clone(),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        let verifier_commands = vec![format!("\"{}\"", verifier.display())];

        let run = runner
            .run(
                &RepairRequest {
                    localization_run_id: report.id,
                    preview_id: preview.id,
                    change_id: CHANGE_ID.to_string(),
                    task_id: TASK_ID.to_string(),
                },
                policy(),
                &dispatcher,
                &verifier_commands,
            )
            .await
            .unwrap();

        assert!(
            matches!(
                run.outcome,
                LocalRepairOutcome::Promoted {
                    attempt_number: 3,
                    ..
                }
            ),
            "unexpected outcome: {:?}; state: {:?}; digests: {:?}",
            run.outcome,
            run.state,
            run.failure_digests
        );
        assert_eq!(run.state.disposition(), &AttemptDisposition::Promoted);
        assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 3);
        assert!(dispatcher.calls.load(Ordering::SeqCst) <= 3);
        assert_eq!(frontier_calls.load(Ordering::SeqCst), 0);
        assert_eq!(run.failure_digests.len(), 2);
        assert_eq!(run.failure_digests[0].attempt_number, 1);
        assert_eq!(run.failure_digests[1].attempt_number, 2);
        assert!(
            dispatcher
                .real_bytes_seen_at_dispatch
                .lock()
                .unwrap()
                .iter()
                .all(|bytes| bytes == ORIGINAL.as_bytes())
        );
        assert_eq!(
            std::fs::read_to_string(root.join(TARGET)).unwrap(),
            "pub const VALUE: &str = \"passing\";\n"
        );
        let prompts = dispatcher.prompts.lock().unwrap();
        assert_eq!(prompts.len(), 3);
        assert!(prompts[1].contains("attempt=1"));
        assert!(prompts[2].contains("attempt=1"));
        assert!(prompts[2].contains("attempt=2"));
        assert_eq!(prompts[2].matches("diagnostic:").count(), 1);

        std::fs::remove_dir_all(root).ok();
    });
}
