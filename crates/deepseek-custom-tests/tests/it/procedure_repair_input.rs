use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use deepseek_custom::procedure::{
    LocalizationAttempt, LocalizationTarget, OpenSpecInput, PatchPreview, PatchPreviewId,
    PatchPreviewStore, ProcedureAttemptDisposition, ProcedurePathState, ProcedureReportRepository,
    ProcedureReviewDisposition, ProcedureRun, ProcedureRunId, ProcedureScratchpad, ProcedureStage,
    ProcedureTask, ProcedureTerminalDisposition, RepairInputError, RepairInputGate, RepairRequest,
    RouteDecision, RouteOverride, RouteTier, ValidatedRepairInput, capture_path_fingerprint,
    sha256_json,
};

const CHANGE_ID: &str = "fixture-change";
const TASK_ID: &str = "1.1";
const TARGET: &str = "src/lib.rs";

fn temp_dir(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "dsc-procedure-repair-input-{tag}-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn write_fake_openspec(root: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let path = root.join("fake-openspec.cmd");
        std::fs::write(&path, "@echo off\r\nexit /b 0\r\n").unwrap();
        path
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;

        let path = root.join("fake-openspec");
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        path
    }
}

fn write_fixture(root: &Path) -> PathBuf {
    let command = write_fake_openspec(root);
    let change = root.join("openspec/changes").join(CHANGE_ID);
    let spec = change.join("specs/sample/capability");
    std::fs::create_dir_all(&spec).unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join(TARGET), "pub fn target_symbol() {}\n").unwrap();
    std::fs::write(
        change.join("proposal.md"),
        "## Why\n\nRepair one fixture.\n\n## What Changes\n\n- Repair one target.\n",
    )
    .unwrap();
    std::fs::write(
        change.join("tasks.md"),
        "- [ ] 1.1 Repair the fixture\n  <!-- covers: sample/capability :: Repair requirement :: Repair scenario -->\n",
    )
    .unwrap();
    std::fs::write(
        spec.join("spec.md"),
        "## Purpose\n\nFixture.\n\n## ADDED Requirements\n\n### Requirement: Repair requirement\nThe system SHALL repair.\n\n#### Scenario: Repair scenario\n- **WHEN** repair starts\n- **THEN** validate its input\n",
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
    let validated = OpenSpecInput::with_command(root, command.display().to_string())
        .validate_and_select_task(CHANGE_ID, TASK_ID)
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
        repository_fingerprint: Some("sha256:repair-fixture".to_string()),
        validation: Some(validated.validation),
        scratchpad: ProcedureScratchpad::default(),
        stage: ProcedureStage::Finished,
        attempts: vec![LocalizationAttempt {
            number: 1,
            backend: "fixture-localizer".to_string(),
            model: "fixture-model".to_string(),
            disposition: ProcedureAttemptDisposition::Accepted,
            targets: vec![LocalizationTarget {
                path: TARGET.to_string(),
                symbol: Some("target_symbol".to_string()),
                evidence: "The fixture source owns the requested edit.".to_string(),
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
        targets: vec![TARGET.to_string()],
        rationale: "Repair the localized target.".to_string(),
        unified_diff: concat!(
            "diff --git a/src/lib.rs b/src/lib.rs\n",
            "--- a/src/lib.rs\n",
            "+++ b/src/lib.rs\n",
            "@@ -1 +1 @@\n",
            "-pub fn target_symbol() {}\n",
            "+pub fn repaired_symbol() {}\n",
        )
        .to_string(),
    };
    PatchPreviewStore::for_project(root).save(&preview).unwrap();
    preview
}

fn gate(root: &Path, command: &Path) -> RepairInputGate {
    RepairInputGate::new(
        OpenSpecInput::with_command(root, command.display().to_string()),
        root.to_path_buf(),
        ProcedureReportRepository::for_project(root),
    )
}

fn request(report: &ProcedureRun, preview: &PatchPreview) -> RepairRequest {
    RepairRequest {
        localization_run_id: report.id,
        preview_id: preview.id,
        change_id: report.change_id.clone(),
        task_id: report.selected_task.id.clone(),
    }
}

#[derive(Default)]
struct RepairActions {
    parser_retry: AtomicUsize,
    patch: AtomicUsize,
    verifier: AtomicUsize,
    local_model: AtomicUsize,
    frontier_model: AtomicUsize,
}

fn assert_actions_untouched(actions: &RepairActions) {
    assert_eq!(actions.parser_retry.load(Ordering::SeqCst), 0);
    assert_eq!(actions.patch.load(Ordering::SeqCst), 0);
    assert_eq!(actions.verifier.load(Ordering::SeqCst), 0);
    assert_eq!(actions.local_model.load(Ordering::SeqCst), 0);
    assert_eq!(actions.frontier_model.load(Ordering::SeqCst), 0);
}

fn pass_gate_then_construct_repair(
    gate: &RepairInputGate,
    request: &RepairRequest,
    actions: &RepairActions,
) -> Result<ValidatedRepairInput, RepairInputError> {
    let validated = gate.load(request)?;
    actions.parser_retry.fetch_add(1, Ordering::SeqCst);
    actions.patch.fetch_add(1, Ordering::SeqCst);
    actions.verifier.fetch_add(1, Ordering::SeqCst);
    actions.local_model.fetch_add(1, Ordering::SeqCst);
    actions.frontier_model.fetch_add(1, Ordering::SeqCst);
    Ok(validated)
}

// covers: deepseek-custom/bounded-repair-escalation :: Repair requires its named approved localization report :: Approved matching report enters the repair ladder
#[test]
fn approved_matching_report_and_patch_state_enter_the_repair_ladder() {
    let root = temp_dir("approved");
    let command = write_fixture(&root);
    let report = save_report(
        &root,
        &command,
        ProcedureReviewDisposition::Approved,
        CHANGE_ID,
        TASK_ID,
    );
    let preview = save_preview(&root, &report);
    let actions = RepairActions::default();

    let validated = pass_gate_then_construct_repair(
        &gate(&root, &command),
        &request(&report, &preview),
        &actions,
    )
    .unwrap();

    assert_eq!(validated.report.run, report);
    assert_eq!(validated.preview, preview);
    assert_eq!(validated.promotion_baseline.fingerprints().len(), 1);
    assert_eq!(validated.promotion_baseline.fingerprints()[0].path, TARGET);
    assert_eq!(
        validated.promotion_baseline.fingerprints()[0].state,
        ProcedurePathState::Present
    );
    assert_eq!(actions.parser_retry.load(Ordering::SeqCst), 1);
    assert_eq!(actions.patch.load(Ordering::SeqCst), 1);
    assert_eq!(actions.verifier.load(Ordering::SeqCst), 1);
    assert_eq!(actions.local_model.load(Ordering::SeqCst), 1);
    assert_eq!(actions.frontier_model.load(Ordering::SeqCst), 1);

    std::fs::remove_dir_all(root).ok();
}

#[derive(Debug, Clone, Copy)]
enum RejectionCase {
    Pending,
    Rejected,
    LegacyUnreviewed,
    MissingReport,
    StaleLocalization,
    ReportChangeMismatch,
    ReportTaskMismatch,
    MissingPreview,
    PreviewRunMismatch,
    PreviewChangeMismatch,
    PreviewTaskMismatch,
    PreviewTargetsMismatch,
    PreviewBaselineMissing,
    PreviewBaselineTargetsMismatch,
    PreviewBaselineStale,
}

impl RejectionCase {
    fn tag(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Rejected => "rejected",
            Self::LegacyUnreviewed => "legacy-unreviewed",
            Self::MissingReport => "missing-report",
            Self::StaleLocalization => "stale-localization",
            Self::ReportChangeMismatch => "report-change-mismatch",
            Self::ReportTaskMismatch => "report-task-mismatch",
            Self::MissingPreview => "missing-preview",
            Self::PreviewRunMismatch => "preview-run-mismatch",
            Self::PreviewChangeMismatch => "preview-change-mismatch",
            Self::PreviewTaskMismatch => "preview-task-mismatch",
            Self::PreviewTargetsMismatch => "preview-targets-mismatch",
            Self::PreviewBaselineMissing => "preview-baseline-missing",
            Self::PreviewBaselineTargetsMismatch => "preview-baseline-targets-mismatch",
            Self::PreviewBaselineStale => "preview-baseline-stale",
        }
    }
}

fn edit_preview_document(
    root: &Path,
    preview_id: PatchPreviewId,
    edit: impl FnOnce(&mut serde_json::Value),
) {
    let path = PatchPreviewStore::for_project(root).report_path(preview_id);
    let mut document: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    edit(&mut document);
    std::fs::write(path, serde_json::to_vec_pretty(&document).unwrap()).unwrap();
}

fn refresh_report_target_fingerprint(root: &Path, report: &ProcedureRun) {
    let store = ProcedureReportRepository::for_project(root);
    let path = store.report_path(&report.id);
    let mut document: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    document["input_fingerprints"]["targets"] =
        serde_json::json!([capture_path_fingerprint(root, TARGET).unwrap()]);
    std::fs::write(path, serde_json::to_vec_pretty(&document).unwrap()).unwrap();
}

fn approved_fixture(root: &Path, command: &Path) -> (ProcedureRun, PatchPreview) {
    let report = save_report(
        root,
        command,
        ProcedureReviewDisposition::Approved,
        CHANGE_ID,
        TASK_ID,
    );
    let preview = save_preview(root, &report);
    (report, preview)
}

fn prepare_rejection(case: RejectionCase) -> (PathBuf, PathBuf, RepairRequest, String) {
    let root = temp_dir(case.tag());
    let command = write_fixture(&root);
    let preview_id = PatchPreviewId::new();

    let (request, expected) = match case {
        RejectionCase::Pending | RejectionCase::Rejected | RejectionCase::LegacyUnreviewed => {
            let disposition = match case {
                RejectionCase::Pending => ProcedureReviewDisposition::Pending,
                RejectionCase::Rejected => ProcedureReviewDisposition::Rejected,
                RejectionCase::LegacyUnreviewed => ProcedureReviewDisposition::LegacyUnreviewed,
                _ => unreachable!(),
            };
            let report = save_report(&root, &command, disposition, CHANGE_ID, TASK_ID);
            (
                RepairRequest {
                    localization_run_id: report.id,
                    preview_id,
                    change_id: CHANGE_ID.to_string(),
                    task_id: TASK_ID.to_string(),
                },
                format!(
                    "patch preview rejected for localization run {}: review disposition is {}",
                    report.id.as_str(),
                    disposition
                ),
            )
        }
        RejectionCase::MissingReport => {
            let run_id = ProcedureRunId::new();
            (
                RepairRequest {
                    localization_run_id: run_id,
                    preview_id,
                    change_id: CHANGE_ID.to_string(),
                    task_id: TASK_ID.to_string(),
                },
                format!(
                    "patch preview rejected for localization run {}: report is missing",
                    run_id.as_str()
                ),
            )
        }
        RejectionCase::StaleLocalization => {
            let (report, preview) = approved_fixture(&root, &command);
            std::fs::write(
                root.join(TARGET),
                "pub fn target_symbol() { println!(\"changed\"); }\n",
            )
            .unwrap();
            (
                request(&report, &preview),
                format!(
                    "patch preview rejected for localization run {}: localization input is stale:\n- {TARGET}\nrun localization again before previewing a patch",
                    report.id.as_str()
                ),
            )
        }
        RejectionCase::ReportChangeMismatch => {
            let report = save_report(
                &root,
                &command,
                ProcedureReviewDisposition::Approved,
                "other-change",
                TASK_ID,
            );
            (
                RepairRequest {
                    localization_run_id: report.id,
                    preview_id,
                    change_id: CHANGE_ID.to_string(),
                    task_id: TASK_ID.to_string(),
                },
                format!(
                    "patch preview rejected for localization run {}: change mismatch; requested `{CHANGE_ID}`, report belongs to `other-change`",
                    report.id.as_str()
                ),
            )
        }
        RejectionCase::ReportTaskMismatch => {
            let report = save_report(
                &root,
                &command,
                ProcedureReviewDisposition::Approved,
                CHANGE_ID,
                "9.9",
            );
            (
                RepairRequest {
                    localization_run_id: report.id,
                    preview_id,
                    change_id: CHANGE_ID.to_string(),
                    task_id: TASK_ID.to_string(),
                },
                format!(
                    "patch preview rejected for localization run {}: task mismatch; requested `{TASK_ID}`, report belongs to `9.9`",
                    report.id.as_str()
                ),
            )
        }
        RejectionCase::MissingPreview => {
            let report = save_report(
                &root,
                &command,
                ProcedureReviewDisposition::Approved,
                CHANGE_ID,
                TASK_ID,
            );
            (
                RepairRequest {
                    localization_run_id: report.id,
                    preview_id,
                    change_id: CHANGE_ID.to_string(),
                    task_id: TASK_ID.to_string(),
                },
                format!(
                    "verification setup rejected for patch preview {}: preview is missing",
                    preview_id.as_str()
                ),
            )
        }
        RejectionCase::PreviewRunMismatch => {
            let (report, mut preview) = approved_fixture(&root, &command);
            preview.localization_run_id = ProcedureRunId::new();
            PatchPreviewStore::for_project(&root)
                .save(&preview)
                .unwrap();
            (
                request(&report, &preview),
                format!(
                    "verification setup rejected for patch preview {}: localization run mismatch; requested `{}`, preview belongs to `{}`",
                    preview.id.as_str(),
                    report.id.as_str(),
                    preview.localization_run_id.as_str()
                ),
            )
        }
        RejectionCase::PreviewChangeMismatch => {
            let (report, mut preview) = approved_fixture(&root, &command);
            preview.change_id = "other-change".to_string();
            PatchPreviewStore::for_project(&root)
                .save(&preview)
                .unwrap();
            (
                request(&report, &preview),
                format!(
                    "verification setup rejected for patch preview {}: change mismatch; requested `{CHANGE_ID}`, preview belongs to `other-change`",
                    preview.id.as_str()
                ),
            )
        }
        RejectionCase::PreviewTaskMismatch => {
            let (report, mut preview) = approved_fixture(&root, &command);
            preview.task_id = "9.9".to_string();
            PatchPreviewStore::for_project(&root)
                .save(&preview)
                .unwrap();
            (
                request(&report, &preview),
                format!(
                    "verification setup rejected for patch preview {}: task mismatch; requested `{TASK_ID}`, preview belongs to `9.9`",
                    preview.id.as_str()
                ),
            )
        }
        RejectionCase::PreviewTargetsMismatch => {
            let (report, preview) = approved_fixture(&root, &command);
            edit_preview_document(&root, preview.id, |document| {
                document["targets"] = serde_json::json!(["src/other.rs"]);
            });
            (
                request(&report, &preview),
                format!(
                    "verification setup rejected for patch preview {}: preview targets do not match the approved localization report; expected [\"{TARGET}\"], actual [\"src/other.rs\"]",
                    preview.id.as_str()
                ),
            )
        }
        RejectionCase::PreviewBaselineMissing => {
            let (report, preview) = approved_fixture(&root, &command);
            edit_preview_document(&root, preview.id, |document| {
                document
                    .as_object_mut()
                    .unwrap()
                    .remove("promotion_baseline");
            });
            (
                request(&report, &preview),
                format!(
                    "verification setup rejected for patch preview {}: preview has no promotion baseline; generate a new preview before applying",
                    preview.id.as_str()
                ),
            )
        }
        RejectionCase::PreviewBaselineTargetsMismatch => {
            let (report, preview) = approved_fixture(&root, &command);
            edit_preview_document(&root, preview.id, |document| {
                document["promotion_baseline"]["fingerprints"] = serde_json::json!([]);
            });
            (
                request(&report, &preview),
                format!(
                    "verification setup rejected for patch preview {}: promotion baseline endpoints do not match preview targets; expected [\"{TARGET}\"], actual []",
                    preview.id.as_str()
                ),
            )
        }
        RejectionCase::PreviewBaselineStale => {
            let (report, preview) = approved_fixture(&root, &command);
            std::fs::write(
                root.join(TARGET),
                "pub fn target_symbol() { println!(\"changed\"); }\n",
            )
            .unwrap();
            refresh_report_target_fingerprint(&root, &report);
            (
                request(&report, &preview),
                format!(
                    "verification setup rejected for patch preview {}: promotion baseline is stale for paths: [\"{TARGET}\"]",
                    preview.id.as_str()
                ),
            )
        }
    };

    (root, command, request, expected)
}

// covers: deepseek-custom/bounded-repair-escalation :: Repair requires its named approved localization report :: Untrusted localization input stops repair
#[test]
fn untrusted_localization_input_stops_repair_before_every_action() {
    let cases = [
        RejectionCase::Pending,
        RejectionCase::Rejected,
        RejectionCase::LegacyUnreviewed,
        RejectionCase::MissingReport,
        RejectionCase::StaleLocalization,
        RejectionCase::ReportChangeMismatch,
        RejectionCase::ReportTaskMismatch,
        RejectionCase::MissingPreview,
        RejectionCase::PreviewRunMismatch,
        RejectionCase::PreviewChangeMismatch,
        RejectionCase::PreviewTaskMismatch,
        RejectionCase::PreviewTargetsMismatch,
        RejectionCase::PreviewBaselineMissing,
        RejectionCase::PreviewBaselineTargetsMismatch,
        RejectionCase::PreviewBaselineStale,
    ];

    for case in cases {
        let (root, command, request, expected) = prepare_rejection(case);
        let actions = RepairActions::default();

        let error = pass_gate_then_construct_repair(&gate(&root, &command), &request, &actions)
            .unwrap_err();

        assert_eq!(error.to_string(), expected, "case: {case:?}");
        assert_actions_untouched(&actions);
        std::fs::remove_dir_all(root).ok();
    }
}
