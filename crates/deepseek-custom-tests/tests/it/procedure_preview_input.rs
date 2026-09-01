use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use deepseek_custom::procedure::{
    LocalizationAttempt, LocalizationTarget, OpenSpecInput, PatchPreviewInputError,
    PatchPreviewInputGate, PatchPreviewInputRequest, ProcedureAttemptDisposition,
    ProcedureReportRepository, ProcedureReviewDisposition, ProcedureRun, ProcedureRunId,
    ProcedureScratchpad, ProcedureStage, ProcedureTerminalDisposition, RouteOverride,
    SamplingInputError, SamplingInputGate, SamplingInputRequest, sha256_json,
};

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "dsc-procedure-preview-input-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn write_fake_openspec(root: &Path) -> PathBuf {
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

fn write_fixture(root: &Path) -> PathBuf {
    let command = write_fake_openspec(root);
    let change = root.join("openspec/changes/fixture-change");
    let spec = change.join("specs/sample/capability");
    std::fs::create_dir_all(&spec).unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn target_symbol() {}\n").unwrap();
    std::fs::write(
        change.join("proposal.md"),
        "## Why\n\nPreview a localized edit.\n\n## What Changes\n\n- Add the preview gate.\n",
    )
    .unwrap();
    std::fs::write(
        change.join("tasks.md"),
        "- [ ] 1.1 Preview the localized edit\n  <!-- covers: sample/capability :: Preview requirement :: Preview scenario -->\n",
    )
    .unwrap();
    std::fs::write(
        spec.join("spec.md"),
        "## Purpose\n\nFixture.\n\n## ADDED Requirements\n\n### Requirement: Preview requirement\nThe system SHALL preview.\n\n#### Scenario: Preview scenario\n- **WHEN** preview starts\n- **THEN** the input is checked\n",
    )
    .unwrap();
    command
}

fn save_report(
    root: &Path,
    command: &Path,
    disposition: ProcedureReviewDisposition,
    report_change: &str,
    report_task: &str,
) -> ProcedureRun {
    let input = OpenSpecInput::with_command(root, command.display().to_string());
    let validated = input
        .validate_and_select_task("fixture-change", "1.1")
        .unwrap();
    let report = ProcedureRun {
        id: ProcedureRunId::new(),
        change_id: report_change.to_string(),
        selected_task: deepseek_custom::procedure::ProcedureTask {
            id: report_task.to_string(),
            text: validated.contract.task.text.clone(),
            covers: validated.contract.task.covers.clone(),
        },
        spec_fingerprint: Some(sha256_json(&validated.contract).unwrap()),
        repository_fingerprint: Some("sha256:repository-fixture".to_string()),
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
    ProcedureReportRepository::for_project(root)
        .save(&report)
        .unwrap();
    report
}

fn request(run_id: ProcedureRunId, change_id: &str, task_id: &str) -> PatchPreviewInputRequest {
    PatchPreviewInputRequest {
        localization_run_id: run_id,
        change_id: change_id.to_string(),
        task_id: task_id.to_string(),
        route_override: RouteOverride::Automatic,
    }
}

fn gate(root: &Path, command: &Path) -> PatchPreviewInputGate {
    PatchPreviewInputGate::new(
        OpenSpecInput::with_command(root, command.display().to_string()),
        root.to_path_buf(),
        ProcedureReportRepository::for_project(root),
    )
}

fn sampling_request(
    run_id: ProcedureRunId,
    change_id: &str,
    task_id: &str,
) -> SamplingInputRequest {
    SamplingInputRequest {
        baseline_localization_run_id: run_id,
        change_id: change_id.to_string(),
        task_id: task_id.to_string(),
    }
}

fn sampling_gate(root: &Path, command: &Path) -> SamplingInputGate {
    SamplingInputGate::new(
        OpenSpecInput::with_command(root, command.display().to_string()),
        root.to_path_buf(),
        ProcedureReportRepository::for_project(root),
    )
}

#[derive(Default)]
struct DownstreamCalls {
    route: AtomicUsize,
    workspace: AtomicUsize,
    dispatch: AtomicUsize,
}

fn pass_gate_then_touch_downstream(
    gate: &PatchPreviewInputGate,
    request: &PatchPreviewInputRequest,
    calls: &DownstreamCalls,
) -> Result<(), PatchPreviewInputError> {
    gate.load(request)?;
    calls.route.fetch_add(1, Ordering::SeqCst);
    calls.workspace.fetch_add(1, Ordering::SeqCst);
    calls.dispatch.fetch_add(1, Ordering::SeqCst);
    Ok(())
}

fn assert_downstream_untouched(calls: &DownstreamCalls) {
    assert_eq!(calls.route.load(Ordering::SeqCst), 0);
    assert_eq!(calls.workspace.load(Ordering::SeqCst), 0);
    assert_eq!(calls.dispatch.load(Ordering::SeqCst), 0);
}

#[derive(Default)]
struct SamplingCalls {
    sampling: AtomicUsize,
    candidate: AtomicUsize,
    patch: AtomicUsize,
    verifier: AtomicUsize,
    model: AtomicUsize,
}

fn pass_sampling_gate_then_start_sampling(
    gate: &SamplingInputGate,
    request: &SamplingInputRequest,
    calls: &SamplingCalls,
) -> Result<(), SamplingInputError> {
    let input = gate.load(request)?;
    assert_eq!(input.report.id, request.baseline_localization_run_id);
    assert_eq!(input.report.change_id, request.change_id);
    assert_eq!(input.report.selected_task.id, request.task_id);
    calls.sampling.fetch_add(1, Ordering::SeqCst);
    Ok(())
}

fn assert_sampling_work_untouched(calls: &SamplingCalls) {
    assert_eq!(calls.sampling.load(Ordering::SeqCst), 0);
    assert_eq!(calls.candidate.load(Ordering::SeqCst), 0);
    assert_eq!(calls.patch.load(Ordering::SeqCst), 0);
    assert_eq!(calls.verifier.load(Ordering::SeqCst), 0);
    assert_eq!(calls.model.load(Ordering::SeqCst), 0);
}

// covers: deepseek-custom/routing-sampling-and-metrics :: Sampling requires its named approved localization report :: Approved matching report enters sampling
#[test]
fn current_named_approved_report_enters_sampling() {
    let root = temp_dir("sampling-approved");
    let command = write_fixture(&root);
    let report = save_report(
        &root,
        &command,
        ProcedureReviewDisposition::Approved,
        "fixture-change",
        "1.1",
    );
    let calls = SamplingCalls::default();

    pass_sampling_gate_then_start_sampling(
        &sampling_gate(&root, &command),
        &sampling_request(report.id, "fixture-change", "1.1"),
        &calls,
    )
    .unwrap();

    assert_eq!(calls.sampling.load(Ordering::SeqCst), 1);
    assert_eq!(calls.candidate.load(Ordering::SeqCst), 0);
    assert_eq!(calls.patch.load(Ordering::SeqCst), 0);
    assert_eq!(calls.verifier.load(Ordering::SeqCst), 0);
    assert_eq!(calls.model.load(Ordering::SeqCst), 0);
    std::fs::remove_dir_all(root).ok();
}

// covers: deepseek-custom/routing-sampling-and-metrics :: Sampling requires its named approved localization report :: Untrusted localization input stops sampling
#[test]
fn untrusted_named_report_stops_before_sampling_or_downstream_work() {
    let root = temp_dir("sampling-rejections");
    let command = write_fixture(&root);

    for disposition in [
        ProcedureReviewDisposition::Pending,
        ProcedureReviewDisposition::Rejected,
        ProcedureReviewDisposition::LegacyUnreviewed,
    ] {
        let report = save_report(&root, &command, disposition, "fixture-change", "1.1");
        let calls = SamplingCalls::default();
        let error = pass_sampling_gate_then_start_sampling(
            &sampling_gate(&root, &command),
            &sampling_request(report.id, "fixture-change", "1.1"),
            &calls,
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            format!(
                "sampling rejected for localization run {}: review disposition is {}",
                report.id.as_str(),
                disposition
            )
        );
        assert_sampling_work_untouched(&calls);
    }

    let missing_id = ProcedureRunId::new();
    let missing_calls = SamplingCalls::default();
    let missing = pass_sampling_gate_then_start_sampling(
        &sampling_gate(&root, &command),
        &sampling_request(missing_id, "fixture-change", "1.1"),
        &missing_calls,
    )
    .unwrap_err();
    assert_eq!(
        missing.to_string(),
        format!(
            "sampling rejected for localization run {}: report is missing",
            missing_id.as_str()
        )
    );
    assert_sampling_work_untouched(&missing_calls);

    let wrong_change = save_report(
        &root,
        &command,
        ProcedureReviewDisposition::Approved,
        "other-change",
        "1.1",
    );
    let wrong_change_calls = SamplingCalls::default();
    let wrong_change_error = pass_sampling_gate_then_start_sampling(
        &sampling_gate(&root, &command),
        &sampling_request(wrong_change.id, "fixture-change", "1.1"),
        &wrong_change_calls,
    )
    .unwrap_err();
    assert_eq!(
        wrong_change_error.to_string(),
        format!(
            "sampling rejected for localization run {}: change mismatch; requested `fixture-change`, report belongs to `other-change`",
            wrong_change.id.as_str()
        )
    );
    assert_sampling_work_untouched(&wrong_change_calls);

    let wrong_task = save_report(
        &root,
        &command,
        ProcedureReviewDisposition::Approved,
        "fixture-change",
        "9.9",
    );
    let wrong_task_calls = SamplingCalls::default();
    let wrong_task_error = pass_sampling_gate_then_start_sampling(
        &sampling_gate(&root, &command),
        &sampling_request(wrong_task.id, "fixture-change", "1.1"),
        &wrong_task_calls,
    )
    .unwrap_err();
    assert_eq!(
        wrong_task_error.to_string(),
        format!(
            "sampling rejected for localization run {}: task mismatch; requested `1.1`, report belongs to `9.9`",
            wrong_task.id.as_str()
        )
    );
    assert_sampling_work_untouched(&wrong_task_calls);

    let stale = save_report(
        &root,
        &command,
        ProcedureReviewDisposition::Approved,
        "fixture-change",
        "1.1",
    );
    std::fs::write(
        root.join("openspec/changes/fixture-change/proposal.md"),
        "## Why\n\nThe selected proposal changed.\n\n## What Changes\n\n- Add sampling.\n",
    )
    .unwrap();
    let stale_calls = SamplingCalls::default();
    let stale_error = pass_sampling_gate_then_start_sampling(
        &sampling_gate(&root, &command),
        &sampling_request(stale.id, "fixture-change", "1.1"),
        &stale_calls,
    )
    .unwrap_err();
    assert_eq!(
        stale_error.to_string(),
        format!(
            "sampling rejected for localization run {}: localization input is stale:\n- openspec/changes/fixture-change/proposal.md\nrun localization again before sampling",
            stale.id.as_str()
        )
    );
    assert_sampling_work_untouched(&stale_calls);

    std::fs::remove_dir_all(root).ok();
}

// covers: deepseek-custom/routed-patch-preview :: Patch preview requires a current localization report :: Current report is accepted
#[test]
fn current_named_approved_report_reaches_route_evaluation() {
    let root = temp_dir("accepted");
    let command = write_fixture(&root);
    let report = save_report(
        &root,
        &command,
        ProcedureReviewDisposition::Approved,
        "fixture-change",
        "1.1",
    );
    let calls = DownstreamCalls::default();

    pass_gate_then_touch_downstream(
        &gate(&root, &command),
        &request(report.id, "fixture-change", "1.1"),
        &calls,
    )
    .unwrap();

    assert_eq!(calls.route.load(Ordering::SeqCst), 1);
    assert_eq!(calls.workspace.load(Ordering::SeqCst), 1);
    assert_eq!(calls.dispatch.load(Ordering::SeqCst), 1);
    std::fs::remove_dir_all(root).ok();
}

// covers: deepseek-custom/routed-patch-preview :: Patch preview requires a current localization report :: Unapproved or missing report is rejected
#[test]
fn dispositions_missing_run_and_run_mismatches_stop_before_downstream_work() {
    let root = temp_dir("rejections");
    let command = write_fixture(&root);

    for disposition in [
        ProcedureReviewDisposition::Pending,
        ProcedureReviewDisposition::Rejected,
        ProcedureReviewDisposition::LegacyUnreviewed,
    ] {
        let report = save_report(&root, &command, disposition, "fixture-change", "1.1");
        let calls = DownstreamCalls::default();
        let error = pass_gate_then_touch_downstream(
            &gate(&root, &command),
            &request(report.id, "fixture-change", "1.1"),
            &calls,
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "patch preview rejected for localization run {}: review disposition is {}",
                report.id.as_str(),
                disposition
            )
        );
        assert_downstream_untouched(&calls);
    }

    let missing_id = ProcedureRunId::new();
    let missing_calls = DownstreamCalls::default();
    let missing = pass_gate_then_touch_downstream(
        &gate(&root, &command),
        &request(missing_id, "fixture-change", "1.1"),
        &missing_calls,
    )
    .unwrap_err();
    assert_eq!(
        missing.to_string(),
        format!(
            "patch preview rejected for localization run {}: report is missing",
            missing_id.as_str()
        )
    );
    assert_downstream_untouched(&missing_calls);

    let wrong_change = save_report(
        &root,
        &command,
        ProcedureReviewDisposition::Approved,
        "other-change",
        "1.1",
    );
    let wrong_change_calls = DownstreamCalls::default();
    let error = pass_gate_then_touch_downstream(
        &gate(&root, &command),
        &request(wrong_change.id, "fixture-change", "1.1"),
        &wrong_change_calls,
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "patch preview rejected for localization run {}: change mismatch; requested `fixture-change`, report belongs to `other-change`",
            wrong_change.id.as_str()
        )
    );
    assert_downstream_untouched(&wrong_change_calls);

    let wrong_task = save_report(
        &root,
        &command,
        ProcedureReviewDisposition::Approved,
        "fixture-change",
        "9.9",
    );
    let wrong_task_calls = DownstreamCalls::default();
    let error = pass_gate_then_touch_downstream(
        &gate(&root, &command),
        &request(wrong_task.id, "fixture-change", "1.1"),
        &wrong_task_calls,
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "patch preview rejected for localization run {}: task mismatch; requested `1.1`, report belongs to `9.9`",
            wrong_task.id.as_str()
        )
    );
    assert_downstream_untouched(&wrong_task_calls);
    std::fs::remove_dir_all(root).ok();
}

// covers: deepseek-custom/routed-patch-preview :: Patch preview requires a current localization report :: Localization report is stale
#[test]
fn every_stale_input_path_is_reported_before_drafting_dispatch() {
    let root = temp_dir("stale");
    let command = write_fixture(&root);
    let report = save_report(
        &root,
        &command,
        ProcedureReviewDisposition::Approved,
        "fixture-change",
        "1.1",
    );
    std::fs::write(
        root.join("openspec/changes/fixture-change/proposal.md"),
        "## Why\n\nThe selected proposal changed.\n\n## What Changes\n\n- Add the preview gate.\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/lib.rs"),
        "pub fn target_symbol() { println!(\"changed\"); }\n",
    )
    .unwrap();
    let calls = DownstreamCalls::default();

    let error = pass_gate_then_touch_downstream(
        &gate(&root, &command),
        &request(report.id, "fixture-change", "1.1"),
        &calls,
    )
    .unwrap_err();

    assert_eq!(
        error.to_string(),
        format!(
            "patch preview rejected for localization run {}: localization input is stale:\n- openspec/changes/fixture-change/proposal.md\n- src/lib.rs\nrun localization again before previewing a patch",
            report.id.as_str()
        )
    );
    assert_downstream_untouched(&calls);
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn approved_legacy_report_is_readable_but_requires_new_localization() {
    let root = temp_dir("legacy-approved");
    let command = write_fixture(&root);
    let report = save_report(
        &root,
        &command,
        ProcedureReviewDisposition::Approved,
        "fixture-change",
        "1.1",
    );
    let store = ProcedureReportRepository::for_project(&root);
    let report_path = store.report_path(&report.id);
    let mut json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&report_path).unwrap()).unwrap();
    json.as_object_mut().unwrap().remove("input_fingerprints");
    json["spec_fingerprint"] = serde_json::Value::String("fnv1a64:legacy".to_string());
    std::fs::write(&report_path, serde_json::to_string_pretty(&json).unwrap()).unwrap();
    assert!(
        store
            .load_with_fingerprints(&report.id)
            .unwrap()
            .input_fingerprints
            .is_empty()
    );
    let calls = DownstreamCalls::default();

    let error = pass_gate_then_touch_downstream(
        &gate(&root, &command),
        &request(report.id, "fixture-change", "1.1"),
        &calls,
    )
    .unwrap_err();

    assert_eq!(
        error.to_string(),
        format!(
            "patch preview rejected for localization run {}: localization input is stale:\n- openspec/changes/fixture-change/proposal.md\n- openspec/changes/fixture-change/specs/sample/capability/spec.md\n- openspec/changes/fixture-change/tasks.md\n- src/lib.rs\nrun localization again before previewing a patch",
            report.id.as_str()
        )
    );
    assert_downstream_untouched(&calls);
    std::fs::remove_dir_all(root).ok();
}
