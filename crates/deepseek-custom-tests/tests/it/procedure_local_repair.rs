use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use deepseek_custom::config::settings::{BackendConfig, ProcedureSettings, Settings};
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

fn write_failing_verifier(root: &Path) -> PathBuf {
    let command = if cfg!(windows) {
        root.join("failing-verifier.cmd")
    } else {
        root.join("failing-verifier.sh")
    };
    #[cfg(windows)]
    std::fs::write(
        &command,
        "@echo off\r\necho deterministic candidate failure 1>&2\r\nexit /b 7\r\n",
    )
    .unwrap();
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(
            &command,
            "#!/bin/sh\necho 'deterministic candidate failure' >&2\nexit 7\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&command).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&command, permissions).unwrap();
    }
    command
}

fn write_isolation_verifier(root: &Path, counter: &Path, workspaces: &Path) -> String {
    #[cfg(windows)]
    {
        let command = root.join("isolation-verifier.ps1");
        let quote = |path: &Path| path.display().to_string().replace('\'', "''");
        std::fs::write(
            &command,
            format!(
                "$counterPath = '{}'\n$workspacesPath = '{}'\n$count = if (Test-Path -LiteralPath $counterPath) {{ [int](Get-Content -Raw -LiteralPath $counterPath) }} else {{ 0 }}\n$count += 1\nSet-Content -NoNewline -LiteralPath $counterPath -Value $count\nAdd-Content -LiteralPath $workspacesPath -Value (Get-Location).Path\nif (Test-Path -LiteralPath 'repair-poison.txt') {{ Write-Error 'prior poison leaked'; exit 31 }}\n$expected = @('first', 'second', 'third')[$count - 1]\n$content = Get-Content -Raw -LiteralPath 'src/lib.rs'\nif (-not $content.Contains($expected)) {{ Write-Error \"expected clean candidate $expected\"; exit 32 }}\nSet-Content -LiteralPath 'repair-poison.txt' -Value 'failed-attempt residue'\nSet-Content -LiteralPath 'src/lib.rs' -Value 'verifier-mutated residue'\nWrite-Error \"deterministic failure $count\"\nexit 7\n",
                quote(counter),
                quote(workspaces),
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
        let command = root.join("isolation-verifier.sh");
        let quote = |path: &Path| path.display().to_string().replace('\'', "'\\''");
        std::fs::write(
            &command,
            format!(
                "#!/bin/sh\ncounter='{}'\nworkspaces='{}'\ncount=0\n[ ! -f \"$counter\" ] || count=$(cat \"$counter\")\ncount=$((count + 1))\nprintf '%s' \"$count\" > \"$counter\"\npwd >> \"$workspaces\"\nif [ -f repair-poison.txt ]; then echo 'prior poison leaked' >&2; exit 31; fi\ncase $count in 1) expected=first ;; 2) expected=second ;; 3) expected=third ;; *) echo 'unexpected attempt' >&2; exit 33 ;; esac\ngrep -q \"$expected\" src/lib.rs || {{ echo \"expected clean candidate $expected\" >&2; exit 32; }}\nprintf '%s\\n' 'failed-attempt residue' > repair-poison.txt\nprintf '%s\\n' 'verifier-mutated residue' > src/lib.rs\necho \"deterministic failure $count\" >&2\nexit 7\n",
                quote(counter),
                quote(workspaces),
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&command).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&command, permissions).unwrap();
        format!("\"{}\"", command.display())
    }
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
    policy_with_frontier_attempts(0)
}

fn policy_with_frontier_attempts(
    frontier_attempts: u8,
) -> deepseek_custom::config::settings::ValidatedProcedureRepairPolicy {
    let frontier_backend = (frontier_attempts > 0).then(|| "fixture-frontier".to_string());
    let backends = frontier_backend.as_ref().map(|name| {
        HashMap::from([(
            name.clone(),
            BackendConfig::CodexCli {
                model: "fixture-frontier-model".to_string(),
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

// covers: deepseek-custom/bounded-repair-escalation :: Verifier failures have a bounded local repair budget :: Local budget is exhausted
#[test]
fn three_local_failures_exhaust_the_budget_without_a_fourth_dispatch() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let root = temp_dir();
        init_repository(&root);
        write_file(&root, TARGET, ORIGINAL);
        let openspec_command = write_openspec_fixture(&root);
        let verifier = write_failing_verifier(&root);
        let (report, preview) = save_trusted_input(&root, &openspec_command);
        let dispatcher = ScriptedLocalDispatcher::new(
            &root,
            vec![candidate("first"), candidate("second"), candidate("third")],
        );
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

        let run = runner
            .run(
                &RepairRequest {
                    localization_run_id: report.id,
                    preview_id: preview.id,
                    change_id: CHANGE_ID.to_string(),
                    task_id: TASK_ID.to_string(),
                },
                policy_with_frontier_attempts(2),
                &dispatcher,
                &[format!("\"{}\"", verifier.display())],
            )
            .await
            .unwrap();

        assert_eq!(run.outcome, LocalRepairOutcome::LocalExhausted);
        assert_eq!(run.state.disposition(), &AttemptDisposition::LocalExhausted);
        assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 3);
        assert_eq!(run.failure_digests.len(), 3);
        assert_eq!(
            run.failure_digests
                .iter()
                .map(|digest| digest.attempt_number)
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert!(
            run.failure_digests
                .iter()
                .all(|digest| digest.exit_code == Some(7))
        );
        assert_eq!(
            std::fs::read_to_string(root.join(TARGET)).unwrap(),
            ORIGINAL
        );

        std::fs::remove_dir_all(root).ok();
    });
}

#[test]
fn local_exhaustion_blocks_when_frontier_policy_is_disabled() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let root = temp_dir();
        init_repository(&root);
        write_file(&root, TARGET, ORIGINAL);
        let openspec_command = write_openspec_fixture(&root);
        let verifier = write_failing_verifier(&root);
        let (report, preview) = save_trusted_input(&root, &openspec_command);
        let dispatcher = ScriptedLocalDispatcher::new(
            &root,
            vec![candidate("first"), candidate("second"), candidate("third")],
        );
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
                &[format!("\"{}\"", verifier.display())],
            )
            .await
            .unwrap();

        assert_eq!(run.outcome, LocalRepairOutcome::Blocked);
        assert!(matches!(
            run.state.disposition(),
            AttemptDisposition::Blocked { reason }
                if reason == "local repair exhausted and frontier escalation is disabled"
        ));
        assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 3);
        assert_eq!(run.failure_digests.len(), 3);
        assert_eq!(
            std::fs::read_to_string(root.join(TARGET)).unwrap(),
            ORIGINAL
        );

        std::fs::remove_dir_all(root).ok();
    });
}

// covers: deepseek-custom/bounded-repair-escalation :: Every repair starts from a fresh verification workspace :: Prior failed files cannot leak
#[test]
fn failed_candidate_workspaces_are_discarded_before_the_next_attempt() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let root = temp_dir();
        init_repository(&root);
        write_file(&root, TARGET, ORIGINAL);
        let openspec_command = write_openspec_fixture(&root);
        let evidence = temp_dir();
        let counter = evidence.join("isolation-counter.txt");
        let workspaces = evidence.join("isolation-workspaces.txt");
        let verifier = write_isolation_verifier(&root, &counter, &workspaces);
        let (report, preview) = save_trusted_input(&root, &openspec_command);
        let dispatcher = ScriptedLocalDispatcher::new(
            &root,
            vec![candidate("first"), candidate("second"), candidate("third")],
        );
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

        let run = runner
            .run(
                &RepairRequest {
                    localization_run_id: report.id,
                    preview_id: preview.id,
                    change_id: CHANGE_ID.to_string(),
                    task_id: TASK_ID.to_string(),
                },
                policy_with_frontier_attempts(2),
                &dispatcher,
                &[verifier],
            )
            .await
            .unwrap();

        assert_eq!(run.outcome, LocalRepairOutcome::LocalExhausted);
        assert_eq!(std::fs::read_to_string(&counter).unwrap(), "3");
        let attempted_workspaces = std::fs::read_to_string(&workspaces)
            .unwrap()
            .lines()
            .map(PathBuf::from)
            .collect::<Vec<_>>();
        assert_eq!(attempted_workspaces.len(), 3);
        let unique_workspaces = attempted_workspaces
            .iter()
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(unique_workspaces.len(), 3);
        assert!(
            attempted_workspaces
                .iter()
                .all(|workspace| !workspace.exists()),
            "a failed verification workspace was retained: {attempted_workspaces:?}"
        );
        assert_eq!(
            std::fs::read_to_string(root.join(TARGET)).unwrap(),
            ORIGINAL
        );
        assert!(!root.join("repair-poison.txt").exists());

        std::fs::remove_dir_all(root).ok();
        std::fs::remove_dir_all(evidence).ok();
    });
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
