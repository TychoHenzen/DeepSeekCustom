use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use deepseek_custom::procedure::{
    ApplyRequest, LocalizationAttempt, LocalizationTarget, OpenSpecInput, PatchPreview,
    PatchPreviewId, PatchPreviewStore, ProcedureApplyProgress, ProcedureApplyRunner,
    ProcedureAttemptDisposition, ProcedureProgress, ProcedureReportStore,
    ProcedureReviewDisposition, ProcedureRun, ProcedureRunId, ProcedureScratchpad, ProcedureStage,
    ProcedureTask, ProcedureTerminalDisposition, PromotionFailureInjection, RouteDecision,
    RouteOverride, RouteTier, VerificationInputGate, VerifierGateDisposition, sha256_json,
};
use tokio::sync::mpsc;

const UPDATE_DIFF: &str = concat!(
    "diff --git a/src/target.rs b/src/target.rs\n",
    "--- a/src/target.rs\n",
    "+++ b/src/target.rs\n",
    "@@ -1 +1 @@\n",
    "-pub fn value() -> i32 { 1 }\n",
    "+pub fn value() -> i32 { 2 }\n",
);
const CREATE_DIFF: &str = concat!(
    "diff --git a/src/created.rs b/src/created.rs\n",
    "new file mode 100644\n",
    "--- /dev/null\n",
    "+++ b/src/created.rs\n",
    "@@ -0,0 +1 @@\n",
    "+pub const CREATED: bool = true;\n",
);
const DELETE_DIFF: &str = concat!(
    "diff --git a/src/deleted.rs b/src/deleted.rs\n",
    "deleted file mode 100644\n",
    "--- a/src/deleted.rs\n",
    "+++ /dev/null\n",
    "@@ -1 +0,0 @@\n",
    "-pub const DELETED: bool = true;\n",
);
const RENAME_DIFF: &str = concat!(
    "diff --git a/src/old.rs b/src/new.rs\n",
    "similarity index 100%\n",
    "rename from src/old.rs\n",
    "rename to src/new.rs\n",
);
const ROLLBACK_DIFF: &str = concat!(
    "diff --git a/src/target.rs b/src/target.rs\n",
    "--- a/src/target.rs\n",
    "+++ b/src/target.rs\n",
    "@@ -1 +1 @@\n",
    "-pub fn value() -> i32 { 1 }\n",
    "+pub fn value() -> i32 { 2 }\n",
    "diff --git a/src/created.rs b/src/created.rs\n",
    "new file mode 100644\n",
    "--- /dev/null\n",
    "+++ b/src/created.rs\n",
    "@@ -0,0 +1 @@\n",
    "+pub const CREATED: bool = true;\n",
);
const PRACTICAL_DIFF: &str = concat!(
    "diff --git a/src/lib.rs b/src/lib.rs\n",
    "--- a/src/lib.rs\n",
    "+++ b/src/lib.rs\n",
    "@@ -1 +1,3 @@\n",
    "-pub fn value() -> i32 { 1 }\n",
    "+pub fn value() -> i32 {\n",
    "+    2\n",
    "+}\n",
);

struct Fixture {
    root: PathBuf,
    openspec_command: PathBuf,
    verifier_command: PathBuf,
    report: ProcedureRun,
    preview: PatchPreview,
}

fn temp_dir(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("dsc apply {tag} {}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn write_file(root: &Path, relative: &str, bytes: &[u8]) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
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
    let spec_root = root.join("openspec/changes/fixture-change/specs/sample/capability");
    std::fs::create_dir_all(&spec_root).unwrap();
    write_file(
        root,
        "openspec/changes/fixture-change/proposal.md",
        b"## Why\n\nFixture.\n\n## What Changes\n\n- Apply fixture.\n",
    );
    write_file(
        root,
        "openspec/changes/fixture-change/tasks.md",
        b"- [ ] 1.1 Apply fixture\n  <!-- covers: sample/capability :: Apply fixture :: Apply scenario -->\n",
    );
    write_file(
        root,
        "openspec/changes/fixture-change/specs/sample/capability/spec.md",
        b"## Purpose\n\nFixture.\n\n## ADDED Requirements\n\n### Requirement: Apply fixture\nThe system SHALL apply.\n\n#### Scenario: Apply scenario\n- **WHEN** Apply runs\n- **THEN** the patch is verified\n",
    );
    command
}

fn write_verifier_fixture(root: &Path) -> PathBuf {
    let command = if cfg!(windows) {
        root.join("fake-verifier.cmd")
    } else {
        root.join("fake-verifier.sh")
    };
    #[cfg(windows)]
    std::fs::write(
        &command,
        "@echo off\r\nif /I \"%~1\"==\"fail\" (echo verifier failed 1>&2 & exit /b 7)\r\nif /I \"%~1\"==\"write\" echo ran>>\"%~2\"\r\nif /I \"%~1\"==\"wait\" (echo started>\"%~2\" & timeout /t 10 /nobreak >nul)\r\nif /I \"%~1\"==\"stale\" (echo started>\"%~2\" & timeout /t 2 /nobreak >nul)\r\nexit /b 0\r\n",
    )
    .unwrap();
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(
            &command,
            "#!/bin/sh\nif [ \"$1\" = fail ]; then echo 'verifier failed' >&2; exit 7; fi\nif [ \"$1\" = write ]; then printf ran >> \"$2\"; fi\nif [ \"$1\" = wait ]; then printf started > \"$2\"; sleep 10; fi\nif [ \"$1\" = stale ]; then printf started > \"$2\"; sleep 2; fi\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&command).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&command, permissions).unwrap();
    }
    command
}

fn save_fixture(
    tag: &str,
    targets: &[&str],
    diff: &str,
    initial_files: &[(&str, &[u8])],
) -> Fixture {
    let root = temp_dir(tag);
    init_repository(&root);
    for (path, contents) in initial_files {
        write_file(&root, path, contents);
    }
    let openspec_command = write_openspec_fixture(&root);
    let verifier_command = write_verifier_fixture(&root);
    let input = OpenSpecInput::with_command(&root, openspec_command.display().to_string());
    let validated = input
        .validate_and_select_task("fixture-change", "1.1")
        .unwrap();
    let report = ProcedureRun {
        id: ProcedureRunId::new(),
        change_id: "fixture-change".to_string(),
        selected_task: ProcedureTask {
            id: "1.1".to_string(),
            text: validated.contract.task.text.clone(),
            covers: validated.contract.task.covers.clone(),
        },
        spec_fingerprint: Some(sha256_json(&validated.contract).unwrap()),
        repository_fingerprint: Some("sha256:fixture".to_string()),
        validation: Some(validated.validation),
        scratchpad: ProcedureScratchpad::default(),
        stage: ProcedureStage::Finished,
        attempts: vec![LocalizationAttempt {
            number: 1,
            backend: "fixture".to_string(),
            model: "fixture".to_string(),
            disposition: ProcedureAttemptDisposition::Accepted,
            targets: targets
                .iter()
                .map(|path| LocalizationTarget {
                    path: (*path).to_string(),
                    symbol: None,
                    evidence: "fixture target".to_string(),
                })
                .collect(),
            validation_error: None,
        }],
        review_disposition: ProcedureReviewDisposition::Approved,
        terminal_disposition: Some(ProcedureTerminalDisposition::AwaitingReview),
    };
    ProcedureReportStore::for_project(&root)
        .save(&report)
        .unwrap();
    let mut preview_targets = targets
        .iter()
        .map(|path| (*path).to_string())
        .collect::<Vec<_>>();
    preview_targets.sort();
    let preview = PatchPreview {
        id: PatchPreviewId::new(),
        localization_run_id: report.id,
        change_id: report.change_id.clone(),
        task_id: report.selected_task.id.clone(),
        route: RouteDecision {
            automatic_tier: RouteTier::Local,
            effective_tier: RouteTier::Local,
            signals: Vec::new(),
            selected_override: RouteOverride::Automatic,
            overridden: false,
        },
        backend: "fixture".to_string(),
        model: "fixture".to_string(),
        targets: preview_targets,
        rationale: "fixture patch".to_string(),
        unified_diff: diff.to_string(),
    };
    PatchPreviewStore::for_project(&root)
        .save(&preview)
        .unwrap();
    Fixture {
        root,
        openspec_command,
        verifier_command,
        report,
        preview,
    }
}

fn request(fixture: &Fixture) -> ApplyRequest {
    ApplyRequest {
        localization_run_id: fixture.report.id,
        preview_id: fixture.preview.id,
        change_id: fixture.preview.change_id.clone(),
        task_id: fixture.preview.task_id.clone(),
    }
}

fn command(script: &Path, action: &str, marker: Option<&Path>) -> String {
    let marker = marker
        .map(|path| format!(" \"{}\"", path.display()))
        .unwrap_or_default();
    format!("\"{}\" {action}{marker}", script.display())
}

fn apply_events(
    receiver: &mut mpsc::UnboundedReceiver<ProcedureProgress>,
) -> Vec<ProcedureApplyProgress> {
    let mut progress = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        if let ProcedureProgress::Apply {
            progress: event, ..
        } = event
        {
            progress.push(*event);
        }
    }
    progress
}

fn run_async(future: impl std::future::Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future);
}

async fn apply(
    fixture: &Fixture,
    commands: Vec<String>,
    injection: Option<PromotionFailureInjection>,
) -> (ProcedureTerminalDisposition, Vec<ProcedureApplyProgress>) {
    let interrupt = Arc::new(AtomicBool::new(false));
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let runner = ProcedureApplyRunner::new(
        VerificationInputGate::new(
            OpenSpecInput::with_command(
                &fixture.root,
                fixture.openspec_command.display().to_string(),
            ),
            fixture.root.clone(),
            ProcedureReportStore::for_project(&fixture.root),
        ),
        fixture.root.clone(),
        interrupt,
    )
    .with_progress(sender);
    let terminal = match injection {
        Some(injection) => runner
            .run_with_failure_injection(
                ProcedureRunId::new(),
                request(fixture),
                &commands,
                injection,
            )
            .await
            .unwrap(),
        None => runner
            .run(ProcedureRunId::new(), request(fixture), &commands)
            .await
            .unwrap(),
    };
    (terminal, apply_events(&mut receiver))
}

fn assert_successful_gates(progress: &[ProcedureApplyProgress], verifier_count: usize) {
    assert_eq!(
        progress
            .iter()
            .filter(|event| matches!(event, ProcedureApplyProgress::PatchGateCompleted { result } if result.success))
            .count(),
        2
    );
    assert_eq!(
        progress
            .iter()
            .filter(|event| matches!(event, ProcedureApplyProgress::VerifierGateCompleted { evidence, .. } if evidence.disposition == VerifierGateDisposition::Passed))
            .count(),
        verifier_count
    );
    assert!(
        progress
            .iter()
            .any(|event| matches!(event, ProcedureApplyProgress::PromotionSucceeded { .. }))
    );
}

#[test]
fn apply_passes_all_gates_and_promotes_only_the_target() {
    run_async(async {
        let fixture = save_fixture(
            "passing",
            &["src/target.rs"],
            UPDATE_DIFF,
            &[
                ("src/target.rs", b"pub fn value() -> i32 { 1 }\n"),
                ("unrelated.bin", &[0, 255, 1, 2]),
            ],
        );
        let unrelated = std::fs::read(fixture.root.join("unrelated.bin")).unwrap();
        let (terminal, progress) = apply(
            &fixture,
            vec![command(&fixture.verifier_command, "pass", None)],
            None,
        )
        .await;
        assert_eq!(terminal, ProcedureTerminalDisposition::Succeeded);
        assert_successful_gates(&progress, 1);
        assert_eq!(
            std::fs::read_to_string(fixture.root.join("src/target.rs")).unwrap(),
            "pub fn value() -> i32 { 2 }\n"
        );
        assert_eq!(
            std::fs::read(fixture.root.join("unrelated.bin")).unwrap(),
            unrelated
        );
        std::fs::remove_dir_all(fixture.root).ok();
    });
}

#[test]
fn apply_failing_gate_stops_later_commands_and_preserves_real_bytes() {
    run_async(async {
        let fixture = save_fixture(
            "failing",
            &["src/target.rs"],
            UPDATE_DIFF,
            &[("src/target.rs", b"pub fn value() -> i32 { 1 }\n")],
        );
        let before = std::fs::read(fixture.root.join("src/target.rs")).unwrap();
        let later = fixture.root.join("later-ran.txt");
        let (terminal, progress) = apply(
            &fixture,
            vec![
                command(&fixture.verifier_command, "fail", None),
                command(&fixture.verifier_command, "write", Some(&later)),
            ],
            None,
        )
        .await;
        assert!(matches!(
            terminal,
            ProcedureTerminalDisposition::Failed { .. }
        ));
        assert_eq!(
            std::fs::read(fixture.root.join("src/target.rs")).unwrap(),
            before
        );
        assert!(!later.exists());
        assert!(progress.iter().any(|event| matches!(
            event,
            ProcedureApplyProgress::VerificationFinished { report }
                if report.stopped_after_failure && report.first_failed_gate == Some(0)
        )));
        std::fs::remove_dir_all(fixture.root).ok();
    });
}

#[test]
fn apply_interrupted_gate_cleans_snapshot_and_never_promotes() {
    run_async(async {
        let fixture = save_fixture(
            "interrupted",
            &["src/target.rs"],
            UPDATE_DIFF,
            &[("src/target.rs", b"pub fn value() -> i32 { 1 }\n")],
        );
        let marker = fixture.root.join("interrupt-started.txt");
        let before = std::fs::read(fixture.root.join("src/target.rs")).unwrap();
        let interrupt = Arc::new(AtomicBool::new(false));
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let runner = ProcedureApplyRunner::new(
            VerificationInputGate::new(
                OpenSpecInput::with_command(
                    &fixture.root,
                    fixture.openspec_command.display().to_string(),
                ),
                fixture.root.clone(),
                ProcedureReportStore::for_project(&fixture.root),
            ),
            fixture.root.clone(),
            Arc::clone(&interrupt),
        )
        .with_progress(sender);
        let commands = [command(&fixture.verifier_command, "wait", Some(&marker))];
        let future = runner.run(ProcedureRunId::new(), request(&fixture), &commands);
        tokio::pin!(future);
        while !marker.exists() {
            tokio::select! {
                terminal = &mut future => panic!("interrupt fixture ended early: {terminal:?}"),
                _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {}
            }
        }
        interrupt.store(true, Ordering::SeqCst);
        let terminal = future.await.unwrap();
        let progress = apply_events(&mut receiver);
        assert_eq!(terminal, ProcedureTerminalDisposition::Interrupted);
        assert!(
            !progress
                .iter()
                .any(|event| matches!(event, ProcedureApplyProgress::PromotionSucceeded { .. }))
        );
        assert_eq!(
            std::fs::read(fixture.root.join("src/target.rs")).unwrap(),
            before
        );
        std::fs::remove_dir_all(fixture.root).ok();
    });
}

#[test]
fn apply_stale_baseline_lists_the_concurrent_target_edit() {
    run_async(async {
        let fixture = save_fixture(
            "stale",
            &["src/target.rs"],
            UPDATE_DIFF,
            &[("src/target.rs", b"pub fn value() -> i32 { 1 }\n")],
        );
        let marker = fixture.root.join("stale-started.txt");
        let interrupt = Arc::new(AtomicBool::new(false));
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let runner = ProcedureApplyRunner::new(
            VerificationInputGate::new(
                OpenSpecInput::with_command(
                    &fixture.root,
                    fixture.openspec_command.display().to_string(),
                ),
                fixture.root.clone(),
                ProcedureReportStore::for_project(&fixture.root),
            ),
            fixture.root.clone(),
            interrupt,
        )
        .with_progress(sender);
        let commands = [command(&fixture.verifier_command, "stale", Some(&marker))];
        let future = runner.run(ProcedureRunId::new(), request(&fixture), &commands);
        tokio::pin!(future);
        while !marker.exists() {
            tokio::select! {
                terminal = &mut future => panic!("stale fixture ended early: {terminal:?}"),
                _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {}
            }
        }
        write_file(
            &fixture.root,
            "src/target.rs",
            b"pub fn value() -> i32 { 99 }\n",
        );
        let terminal = future.await.unwrap();
        let progress = apply_events(&mut receiver);
        assert!(matches!(
            terminal,
            ProcedureTerminalDisposition::Failed { .. }
        ));
        assert!(progress.iter().any(|event| matches!(
            event,
            ProcedureApplyProgress::ConflictDetected { paths }
                if paths.iter().any(|path| path.path == "src/target.rs")
        )));
        assert_eq!(
            std::fs::read_to_string(fixture.root.join("src/target.rs")).unwrap(),
            "pub fn value() -> i32 { 99 }\n"
        );
        std::fs::remove_dir_all(fixture.root).ok();
    });
}

#[test]
fn apply_create_promotes_a_new_endpoint() {
    run_async(async {
        let fixture = save_fixture("create", &["src/created.rs"], CREATE_DIFF, &[]);
        let (terminal, progress) = apply(
            &fixture,
            vec![command(&fixture.verifier_command, "pass", None)],
            None,
        )
        .await;
        assert_eq!(terminal, ProcedureTerminalDisposition::Succeeded);
        assert_successful_gates(&progress, 1);
        assert_eq!(
            std::fs::read_to_string(fixture.root.join("src/created.rs")).unwrap(),
            "pub const CREATED: bool = true;\n"
        );
        std::fs::remove_dir_all(fixture.root).ok();
    });
}

#[test]
fn apply_delete_promotes_a_missing_endpoint() {
    run_async(async {
        let fixture = save_fixture(
            "delete",
            &["src/deleted.rs"],
            DELETE_DIFF,
            &[("src/deleted.rs", b"pub const DELETED: bool = true;\n")],
        );
        let (terminal, progress) = apply(
            &fixture,
            vec![command(&fixture.verifier_command, "pass", None)],
            None,
        )
        .await;
        assert_eq!(terminal, ProcedureTerminalDisposition::Succeeded);
        assert_successful_gates(&progress, 1);
        assert!(!fixture.root.join("src/deleted.rs").exists());
        std::fs::remove_dir_all(fixture.root).ok();
    });
}

#[test]
fn apply_rename_promotes_both_rename_endpoints() {
    run_async(async {
        let fixture = save_fixture(
            "rename",
            &["src/old.rs", "src/new.rs"],
            RENAME_DIFF,
            &[("src/old.rs", b"pub const RENAMED: bool = true;\n")],
        );
        let (terminal, progress) = apply(
            &fixture,
            vec![command(&fixture.verifier_command, "pass", None)],
            None,
        )
        .await;
        assert_eq!(terminal, ProcedureTerminalDisposition::Succeeded);
        assert_successful_gates(&progress, 1);
        assert!(!fixture.root.join("src/old.rs").exists());
        assert_eq!(
            std::fs::read_to_string(fixture.root.join("src/new.rs")).unwrap(),
            "pub const RENAMED: bool = true;\n"
        );
        std::fs::remove_dir_all(fixture.root).ok();
    });
}

#[test]
fn apply_rollback_restores_existing_and_removes_created_paths() {
    run_async(async {
        let fixture = save_fixture(
            "rollback",
            &["src/target.rs", "src/created.rs"],
            ROLLBACK_DIFF,
            &[("src/target.rs", b"pub fn value() -> i32 { 1 }\n")],
        );
        let (terminal, progress) = apply(
            &fixture,
            vec![command(&fixture.verifier_command, "pass", None)],
            Some(PromotionFailureInjection {
                fail_install_at: Some(1),
                fail_rollback_at: None,
            }),
        )
        .await;
        assert!(matches!(
            terminal,
            ProcedureTerminalDisposition::Failed { .. }
        ));
        assert_eq!(
            std::fs::read_to_string(fixture.root.join("src/target.rs")).unwrap(),
            "pub fn value() -> i32 { 1 }\n"
        );
        assert!(!fixture.root.join("src/created.rs").exists());
        assert!(progress.iter().any(|event| matches!(
            event,
            ProcedureApplyProgress::PromotionFailed { recovery: Some(recovery), .. }
                if recovery.rollback_succeeded()
        )));
        std::fs::remove_dir_all(fixture.root).ok();
    });
}

#[test]
fn settings_declares_the_four_workspace_gates_and_practical_patch_promotes() {
    run_async(async {
        let settings_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("settings.json");
        let settings: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(settings_path).unwrap()).unwrap();
        let commands = settings["procedure"]["verifier_commands"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            commands,
            [
                "cargo fmt --all -- --check",
                "cargo check --workspace",
                "cargo clippy --workspace -- -D warnings",
                "cargo test --workspace",
            ]
        );

        let fixture = save_fixture(
            "practical-rust",
            &["src/lib.rs"],
            PRACTICAL_DIFF,
            &[("src/lib.rs", b"pub fn value() -> i32 { 1 }\n")],
        );
        write_file(
            &fixture.root,
            "Cargo.toml",
            b"[package]\nname = \"apply-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        let (terminal, progress) = apply(&fixture, commands, None).await;
        assert_eq!(terminal, ProcedureTerminalDisposition::Succeeded);
        assert_successful_gates(&progress, 4);
        assert_eq!(
            std::fs::read_to_string(fixture.root.join("src/lib.rs")).unwrap(),
            "pub fn value() -> i32 {\n    2\n}\n"
        );
        std::fs::remove_dir_all(fixture.root).ok();
    });
}
