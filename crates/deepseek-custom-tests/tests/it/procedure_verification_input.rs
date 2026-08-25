use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use deepseek_custom::procedure::{
    ApplyRequest, LocalizationAttempt, LocalizationTarget, OpenSpecInput, PatchPreview,
    PatchPreviewId, PatchPreviewInputError, PatchPreviewStore, ProcedureAttemptDisposition,
    ProcedureReportStore, ProcedureReviewDisposition, ProcedureRun, ProcedureRunId,
    ProcedureScratchpad, ProcedureStage, ProcedureTask, ProcedureTerminalDisposition,
    RouteDecision, RouteOverride, RouteTier, VerificationInputError, VerificationInputGate,
    sha256_json,
};

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "dsc-procedure-verification-input-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn write_fixture(root: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let path = root.join("fake-openspec.cmd");
        std::fs::write(&path, "@echo off\r\necho %*\r\nexit /b 0\r\n").unwrap();
        path
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;

        let path = root.join("fake-openspec");
        std::fs::write(&path, "#!/bin/sh\nprintf '%s\\n' \"$*\"\nexit 0\n").unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        path
    }
}

fn setup_fixture(root: &Path) -> PathBuf {
    let command = write_fixture(root);
    let change = root.join("openspec/changes/fixture-change");
    let spec = change.join("specs/sample/capability");
    std::fs::create_dir_all(&spec).unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn target_symbol() {}\n").unwrap();
    std::fs::write(
        change.join("proposal.md"),
        "## Why\n\nFixture.\n\n## What Changes\n\n- Verify the fixture.\n",
    )
    .unwrap();
    std::fs::write(
        change.join("tasks.md"),
        "- [ ] 1.1 Verify the fixture\n  <!-- covers: sample/capability :: Verify requirement :: Verify scenario -->\n",
    )
    .unwrap();
    std::fs::write(
        spec.join("spec.md"),
        "## Purpose\n\nFixture.\n\n## ADDED Requirements\n\n### Requirement: Verify requirement\nThe system SHALL verify.\n\n#### Scenario: Verify scenario\n- **WHEN** verification starts\n- **THEN** the input is checked\n",
    )
    .unwrap();
    command
}

fn save_report(
    root: &Path,
    command: &Path,
    disposition: ProcedureReviewDisposition,
    change_id: &str,
    task_id: &str,
) -> ProcedureRun {
    let input = OpenSpecInput::with_command(root, command.display().to_string());
    let validated = input
        .validate_and_select_task("fixture-change", "1.1")
        .unwrap();
    let report = ProcedureRun {
        id: ProcedureRunId::new(),
        change_id: change_id.to_string(),
        selected_task: ProcedureTask {
            id: task_id.to_string(),
            text: validated.contract.task.text.clone(),
            covers: validated.contract.task.covers.clone(),
        },
        spec_fingerprint: Some(sha256_json(&validated.contract).unwrap()),
        repository_fingerprint: Some("sha256:fixture-repository".to_string()),
        validation: Some(validated.validation),
        scratchpad: ProcedureScratchpad::default(),
        stage: ProcedureStage::Finished,
        attempts: vec![LocalizationAttempt {
            number: 1,
            backend: "fixture-localizer".to_string(),
            model: "fixture-model".to_string(),
            disposition: ProcedureAttemptDisposition::Accepted,
            targets: vec![LocalizationTarget {
                path: "src/lib.rs".to_string(),
                symbol: Some("target_symbol".to_string()),
                evidence: "The fixture source owns the edit.".to_string(),
            }],
            validation_error: None,
        }],
        review_disposition: disposition,
        terminal_disposition: Some(ProcedureTerminalDisposition::AwaitingReview),
    };
    ProcedureReportStore::for_project(root)
        .save(&report)
        .unwrap();
    report
}

fn save_preview(root: &Path, report: &ProcedureRun) -> PatchPreview {
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
        backend: "fixture-backend".to_string(),
        model: "fixture-model".to_string(),
        targets: vec!["src/lib.rs".to_string()],
        rationale: "The fixture target is localized.".to_string(),
        unified_diff: concat!(
            "diff --git a/src/lib.rs b/src/lib.rs\n",
            "--- a/src/lib.rs\n",
            "+++ b/src/lib.rs\n",
            "@@ -1 +1 @@\n",
            "-pub fn target_symbol() {}\n",
            "+pub fn renamed_symbol() {}\n",
        )
        .to_string(),
    };
    PatchPreviewStore::for_project(root).save(&preview).unwrap();
    preview
}

#[test]
fn legacy_preview_without_promotion_baseline_is_rejected_exactly() {
    let root = temp_dir("legacy-preview-baseline");
    let command = setup_fixture(&root);
    let report = save_report(
        &root,
        &command,
        ProcedureReviewDisposition::Approved,
        "fixture-change",
        "1.1",
    );
    let preview = save_preview(&root, &report);
    let path = PatchPreviewStore::for_project(&root).report_path(preview.id);
    let mut document: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    document
        .as_object_mut()
        .unwrap()
        .remove("promotion_baseline");
    std::fs::write(&path, serde_json::to_vec_pretty(&document).unwrap()).unwrap();

    let error = gate(&root, &command)
        .load(&request(&report, &preview))
        .unwrap_err();

    assert_eq!(
        error.to_string(),
        format!(
            "verification setup rejected for patch preview {}: preview has no promotion baseline; generate a new preview before applying",
            preview.id.as_str()
        )
    );
    std::fs::remove_dir_all(root).ok();
}

fn gate(root: &Path, command: &Path) -> VerificationInputGate {
    VerificationInputGate::new(
        OpenSpecInput::with_command(root, command.display().to_string()),
        root.to_path_buf(),
        ProcedureReportStore::for_project(root),
    )
}

fn request(report: &ProcedureRun, preview: &PatchPreview) -> ApplyRequest {
    ApplyRequest {
        localization_run_id: report.id,
        preview_id: preview.id,
        change_id: report.change_id.clone(),
        task_id: report.selected_task.id.clone(),
    }
}

#[test]
fn approved_current_report_and_matching_preview_enter_verification_setup() {
    let root = temp_dir("approved");
    let command = setup_fixture(&root);
    let report = save_report(
        &root,
        &command,
        ProcedureReviewDisposition::Approved,
        "fixture-change",
        "1.1",
    );
    let preview = save_preview(&root, &report);
    let calls = AtomicUsize::new(0);

    let validated = gate(&root, &command)
        .load(&request(&report, &preview))
        .unwrap();
    calls.fetch_add(1, Ordering::SeqCst);

    assert_eq!(validated.report.run, report);
    assert_eq!(validated.preview, preview);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn untrusted_localization_reports_stop_before_any_verification_side_effect() {
    let root = temp_dir("untrusted");
    let command = setup_fixture(&root);
    let calls = AtomicUsize::new(0);

    for disposition in [
        ProcedureReviewDisposition::Pending,
        ProcedureReviewDisposition::Rejected,
        ProcedureReviewDisposition::LegacyUnreviewed,
    ] {
        let report = save_report(&root, &command, disposition, "fixture-change", "1.1");
        let error = gate(&root, &command)
            .load(&ApplyRequest {
                localization_run_id: report.id,
                preview_id: PatchPreviewId::new(),
                change_id: "fixture-change".to_string(),
                task_id: "1.1".to_string(),
            })
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "patch preview rejected for localization run {}: review disposition is {}",
                report.id.as_str(),
                disposition
            )
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    let missing_id = ProcedureRunId::new();
    let error = gate(&root, &command)
        .load(&ApplyRequest {
            localization_run_id: missing_id,
            preview_id: PatchPreviewId::new(),
            change_id: "fixture-change".to_string(),
            task_id: "1.1".to_string(),
        })
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "patch preview rejected for localization run {}: report is missing",
            missing_id.as_str()
        )
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    let stale = save_report(
        &root,
        &command,
        ProcedureReviewDisposition::Approved,
        "fixture-change",
        "1.1",
    );
    let preview = save_preview(&root, &stale);
    std::fs::write(
        root.join("src/lib.rs"),
        "pub fn target_symbol() { println!(\"changed\"); }\n",
    )
    .unwrap();
    let error = gate(&root, &command)
        .load(&request(&stale, &preview))
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "patch preview rejected for localization run {}: localization input is stale:\n- src/lib.rs\nrun localization again before previewing a patch",
            stale.id.as_str()
        )
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn mismatched_report_and_preview_inputs_have_exact_diagnostics_and_no_side_effects() {
    let root = temp_dir("mismatch");
    let command = setup_fixture(&root);
    let calls = AtomicUsize::new(0);

    let wrong_change = save_report(
        &root,
        &command,
        ProcedureReviewDisposition::Approved,
        "other-change",
        "1.1",
    );
    let error = gate(&root, &command)
        .load(&ApplyRequest {
            localization_run_id: wrong_change.id,
            preview_id: PatchPreviewId::new(),
            change_id: "fixture-change".to_string(),
            task_id: "1.1".to_string(),
        })
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "patch preview rejected for localization run {}: change mismatch; requested `fixture-change`, report belongs to `other-change`",
            wrong_change.id.as_str()
        )
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    let wrong_task = save_report(
        &root,
        &command,
        ProcedureReviewDisposition::Approved,
        "fixture-change",
        "9.9",
    );
    let error = gate(&root, &command)
        .load(&ApplyRequest {
            localization_run_id: wrong_task.id,
            preview_id: PatchPreviewId::new(),
            change_id: "fixture-change".to_string(),
            task_id: "1.1".to_string(),
        })
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "patch preview rejected for localization run {}: task mismatch; requested `1.1`, report belongs to `9.9`",
            wrong_task.id.as_str()
        )
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    let report = save_report(
        &root,
        &command,
        ProcedureReviewDisposition::Approved,
        "fixture-change",
        "1.1",
    );
    let mut preview = save_preview(&root, &report);
    preview.localization_run_id = ProcedureRunId::new();
    PatchPreviewStore::for_project(&root)
        .save(&preview)
        .unwrap();
    let error = gate(&root, &command)
        .load(&request(&report, &preview))
        .unwrap_err();
    assert!(matches!(
        &error,
        VerificationInputError::PreviewRunMismatch { .. }
    ));
    assert_eq!(
        error.to_string(),
        format!(
            "verification setup rejected for patch preview {}: localization run mismatch; requested `{}`, preview belongs to `{}`",
            preview.id.as_str(),
            report.id.as_str(),
            preview.localization_run_id.as_str()
        )
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    std::fs::remove_dir_all(root).ok();
}

#[test]
fn missing_preview_stops_before_verification_setup() {
    let root = temp_dir("missing-preview");
    let command = setup_fixture(&root);
    let report = save_report(
        &root,
        &command,
        ProcedureReviewDisposition::Approved,
        "fixture-change",
        "1.1",
    );
    let missing_preview = PatchPreviewId::new();
    let error = gate(&root, &command)
        .load(&ApplyRequest {
            localization_run_id: report.id,
            preview_id: missing_preview,
            change_id: "fixture-change".to_string(),
            task_id: "1.1".to_string(),
        })
        .unwrap_err();

    assert!(matches!(
        &error,
        VerificationInputError::PreviewMissing { .. }
    ));
    assert_eq!(
        error.to_string(),
        format!(
            "verification setup rejected for patch preview {}: preview is missing",
            missing_preview.as_str()
        )
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn verification_error_preserves_existing_localization_error_type() {
    let root = temp_dir("error-type");
    let command = setup_fixture(&root);
    let report = save_report(
        &root,
        &command,
        ProcedureReviewDisposition::Pending,
        "fixture-change",
        "1.1",
    );
    let error = gate(&root, &command)
        .load(&ApplyRequest {
            localization_run_id: report.id,
            preview_id: PatchPreviewId::new(),
            change_id: "fixture-change".to_string(),
            task_id: "1.1".to_string(),
        })
        .unwrap_err();
    assert!(matches!(
        &error,
        VerificationInputError::Localization(PatchPreviewInputError::ReviewDisposition { .. })
    ));
    std::fs::remove_dir_all(root).ok();
}
