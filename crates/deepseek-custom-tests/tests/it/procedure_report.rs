use std::io;
use std::path::PathBuf;

use deepseek_custom::error::HarnessError;
use deepseek_custom::procedure::{
    BoundedVerifierOutput, CandidateEligibility, CandidateIneligibility, GitApplyDisposition,
    GitApplyPhase, GitApplyResult, LocalizationAttempt, LocalizationTarget, OpenSpecValidation,
    PatchGateDisposition, PatchGateEvidence, ProcedureApprovedReportError,
    ProcedureAttemptDisposition, ProcedurePathState, ProcedureReportStore,
    ProcedureReviewDisposition, ProcedureRun, ProcedureRunId, ProcedureScratchpad, ProcedureStage,
    ProcedureTask, ProcedureTerminalDisposition, VerifierCommandDisposition,
    VerifierCommandEvidence, VerifierGateDisposition, VerifierGateEvidence, VerifierReport,
    capture_path_fingerprint, require_approved_report,
};

fn temp_path(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "dsc-procedure-report-{tag}-{}-{nanos}",
        std::process::id()
    ))
}

fn completed_run() -> ProcedureRun {
    ProcedureRun {
        id: ProcedureRunId::new(),
        change_id: "add-procedure-localization-runner".to_string(),
        selected_task: ProcedureTask {
            id: "1.3".to_string(),
            text: "Persist localization reports".to_string(),
            covers: Some("Completed report is inspectable".to_string()),
        },
        spec_fingerprint: Some("spec-fingerprint".to_string()),
        repository_fingerprint: Some("repository-fingerprint".to_string()),
        validation: None,
        scratchpad: ProcedureScratchpad::default(),
        stage: ProcedureStage::Finished,
        attempts: vec![LocalizationAttempt {
            number: 1,
            backend: "ollama".to_string(),
            model: "qwen2.5-coder:7b-instruct-q4_K_M".to_string(),
            disposition: ProcedureAttemptDisposition::Accepted,
            targets: vec![LocalizationTarget {
                path: "crates/deepseek-custom/src/procedure/report.rs".to_string(),
                symbol: Some("ProcedureReportStore".to_string()),
                evidence: "The task requires per-run JSON storage.".to_string(),
            }],
            validation_error: None,
        }],
        review_disposition: ProcedureReviewDisposition::Approved,
        terminal_disposition: Some(ProcedureTerminalDisposition::Succeeded),
    }
}

#[test]
fn save_creates_the_required_project_directory_and_uuid_file() {
    let project_root = temp_path("save-path");
    let store = ProcedureReportStore::for_project(&project_root);
    let report = completed_run();

    store.save(&report).unwrap();

    assert!(
        project_root
            .join(".deepseek/procedure-runs")
            .join(format!("{}.json", report.id.as_str()))
            .is_file()
    );
    std::fs::remove_dir_all(project_root).ok();
}

#[test]
fn load_returns_the_saved_targets_and_dispatch_details() {
    let reports_dir = temp_path("round-trip");
    let store = ProcedureReportStore::new(reports_dir.clone());
    let report = completed_run();
    store.save(&report).unwrap();

    let loaded = store.load(&report.id).unwrap();

    assert_eq!(loaded, report);
    assert_eq!(loaded.attempts[0].backend, "ollama");
    assert_eq!(
        loaded.attempts[0].targets[0].symbol.as_deref(),
        Some("ProcedureReportStore")
    );
    std::fs::remove_dir_all(reports_dir).ok();
}

fn awaiting_review_run() -> ProcedureRun {
    let mut report = completed_run();
    report.review_disposition = ProcedureReviewDisposition::Pending;
    report.terminal_disposition = Some(ProcedureTerminalDisposition::AwaitingReview);
    report
}

#[test]
fn report_store_round_trips_every_review_disposition() {
    let reports_dir = temp_path("review-round-trip");
    let store = ProcedureReportStore::new(reports_dir.clone());

    for disposition in [
        ProcedureReviewDisposition::Pending,
        ProcedureReviewDisposition::Approved,
        ProcedureReviewDisposition::Rejected,
        ProcedureReviewDisposition::LegacyUnreviewed,
    ] {
        let mut report = completed_run();
        report.review_disposition = disposition;
        store.save(&report).unwrap();
        assert_eq!(
            store.load(&report.id).unwrap().review_disposition,
            disposition
        );
    }

    std::fs::remove_dir_all(reports_dir).ok();
}

#[test]
fn report_without_review_disposition_loads_as_legacy_unreviewed() {
    let reports_dir = temp_path("legacy-review");
    let store = ProcedureReportStore::new(reports_dir.clone());
    let report = completed_run();
    let mut json = serde_json::to_value(&report).unwrap();
    json.as_object_mut().unwrap().remove("review_disposition");
    std::fs::create_dir_all(&reports_dir).unwrap();
    std::fs::write(
        store.report_path(&report.id),
        serde_json::to_string_pretty(&json).unwrap(),
    )
    .unwrap();

    let loaded = store.load(&report.id).unwrap();

    assert_eq!(
        loaded.review_disposition,
        ProcedureReviewDisposition::LegacyUnreviewed
    );
    assert_ne!(
        loaded.review_disposition,
        ProcedureReviewDisposition::Approved
    );
    std::fs::remove_dir_all(reports_dir).ok();
}

#[test]
fn legacy_report_without_input_fingerprints_remains_readable() {
    let reports_dir = temp_path("legacy-fingerprints");
    let store = ProcedureReportStore::new(reports_dir.clone());
    let report = completed_run();
    std::fs::create_dir_all(&reports_dir).unwrap();
    std::fs::write(
        store.report_path(&report.id),
        serde_json::to_string_pretty(&report).unwrap(),
    )
    .unwrap();

    let stored = store.load_with_fingerprints(&report.id).unwrap();

    assert_eq!(stored.run, report);
    assert!(stored.input_fingerprints.is_empty());
    std::fs::remove_dir_all(reports_dir).ok();
}

#[test]
fn path_identity_is_stable_for_create_delete_and_rename_endpoints() {
    let root = temp_path("path-identities");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/delete.rs"), "delete source\n").unwrap();
    std::fs::write(root.join("src/rename_from.rs"), "rename source\n").unwrap();

    let create_missing = capture_path_fingerprint(&root, "src/create.rs").unwrap();
    let delete_present = capture_path_fingerprint(&root, "src/delete.rs").unwrap();
    let rename_from_present = capture_path_fingerprint(&root, "src/rename_from.rs").unwrap();
    let rename_to_missing = capture_path_fingerprint(&root, "src/rename_to.rs").unwrap();

    std::fs::write(root.join("src/create.rs"), "created\n").unwrap();
    std::fs::remove_file(root.join("src/delete.rs")).unwrap();
    std::fs::rename(
        root.join("src/rename_from.rs"),
        root.join("src/rename_to.rs"),
    )
    .unwrap();

    let create_present = capture_path_fingerprint(&root, "src\\create.rs").unwrap();
    let delete_missing = capture_path_fingerprint(&root, "src/delete.rs").unwrap();
    let rename_from_missing = capture_path_fingerprint(&root, "src/rename_from.rs").unwrap();
    let rename_to_present = capture_path_fingerprint(&root, "src/rename_to.rs").unwrap();

    assert_eq!(create_missing.path, "src/create.rs");
    assert_eq!(
        create_missing.identity_sha256,
        create_present.identity_sha256
    );
    assert_eq!(
        delete_present.identity_sha256,
        delete_missing.identity_sha256
    );
    assert_eq!(
        rename_from_present.identity_sha256,
        rename_from_missing.identity_sha256
    );
    assert_eq!(
        rename_to_missing.identity_sha256,
        rename_to_present.identity_sha256
    );
    assert_eq!(create_missing.state, ProcedurePathState::Missing);
    assert_eq!(create_present.state, ProcedurePathState::Present);
    assert_eq!(delete_present.state, ProcedurePathState::Present);
    assert_eq!(delete_missing.state, ProcedurePathState::Missing);
    assert!(
        [
            &create_missing,
            &delete_present,
            &rename_from_present,
            &rename_to_missing,
        ]
        .into_iter()
        .all(
            |fingerprint| fingerprint.identity_sha256.starts_with("sha256:")
                && fingerprint.identity_sha256.len() == 71
        )
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn load_from_a_missing_report_directory_returns_not_found() {
    let reports_dir = temp_path("missing");
    let store = ProcedureReportStore::new(reports_dir);

    let error = store.load(&ProcedureRunId::new()).unwrap_err();

    assert!(matches!(
        error,
        HarnessError::Io(error) if error.kind() == io::ErrorKind::NotFound
    ));
}

#[test]
fn corrupt_report_returns_a_parse_error_that_names_the_file() {
    let reports_dir = temp_path("corrupt");
    std::fs::create_dir_all(&reports_dir).unwrap();
    let store = ProcedureReportStore::new(reports_dir.clone());
    let id = ProcedureRunId::new();
    let path = reports_dir.join(format!("{}.json", id.as_str()));
    std::fs::write(&path, "{ invalid json").unwrap();

    let error = store.load(&id).unwrap_err();
    let message = error.to_string();

    assert!(matches!(error, HarnessError::Parse(_)));
    assert!(message.contains("could not parse procedure report"));
    assert!(message.contains(&path.display().to_string()));
    std::fs::remove_dir_all(reports_dir).ok();
}

// covers: deepseek-custom/procedure-localization :: Every localization target exists :: Structurally valid targets are semantically wrong
#[test]
fn rejection_updates_only_the_named_awaiting_review_report_and_fails_the_approved_guard() {
    let reports_dir = temp_path("reject-review");
    let store = ProcedureReportStore::new(reports_dir.clone());
    let pending = awaiting_review_run();
    let untouched = awaiting_review_run();
    store.save(&pending).unwrap();
    store.save(&untouched).unwrap();

    let rejected = store.reject(&pending.id).unwrap();

    assert_eq!(
        rejected.review_disposition,
        ProcedureReviewDisposition::Rejected
    );
    assert_eq!(
        rejected.terminal_disposition,
        Some(ProcedureTerminalDisposition::AwaitingReview)
    );
    assert_eq!(rejected.attempts, pending.attempts);
    assert_eq!(rejected.validation, pending.validation);
    assert_eq!(store.load(&pending.id).unwrap(), rejected);
    assert_eq!(store.load(&untouched.id).unwrap(), untouched);
    assert_eq!(
        require_approved_report(&rejected).unwrap_err().to_string(),
        format!(
            "procedure run {} cannot enter a downstream procedure stage: review disposition is rejected",
            pending.id.as_str()
        )
    );
    assert!(
        !reports_dir
            .join(format!("{}.json.tmp", pending.id.as_str()))
            .exists()
    );

    std::fs::remove_dir_all(reports_dir).ok();
}

// covers: deepseek-custom/procedure-localization :: Every localization target exists :: Structurally valid targets are approved
#[test]
fn approval_preserves_structural_evidence_and_is_the_only_path_through_the_guard() {
    let reports_dir = temp_path("approve-review");
    let store = ProcedureReportStore::new(reports_dir.clone());
    let pending = awaiting_review_run();
    store.save(&pending).unwrap();

    assert!(require_approved_report(&pending).is_err());

    let approved = store.approve(&pending.id).unwrap();
    let consumed = require_approved_report(&approved).unwrap();

    assert!(std::ptr::eq(consumed, &approved));
    assert_eq!(
        approved.review_disposition,
        ProcedureReviewDisposition::Approved
    );
    assert_eq!(approved.attempts, pending.attempts);
    assert_eq!(approved.spec_fingerprint, pending.spec_fingerprint);
    assert_eq!(
        approved.repository_fingerprint,
        pending.repository_fingerprint
    );
    assert_eq!(approved.validation, pending.validation);
    assert_eq!(store.load(&pending.id).unwrap(), approved);

    std::fs::remove_dir_all(reports_dir).ok();
}

#[test]
fn downstream_consumer_guard_accepts_only_approved_reports_without_changing_them() {
    let mut report = awaiting_review_run();

    for disposition in [
        ProcedureReviewDisposition::Pending,
        ProcedureReviewDisposition::Rejected,
        ProcedureReviewDisposition::LegacyUnreviewed,
    ] {
        report.review_disposition = disposition;
        let before = report.clone();

        let error = require_approved_report(&report).unwrap_err();

        assert_eq!(
            error,
            ProcedureApprovedReportError {
                run_id: report.id.as_str(),
                disposition,
            }
        );
        assert_eq!(
            error.to_string(),
            format!(
                "procedure run {} cannot enter a downstream procedure stage: review disposition is {}",
                report.id.as_str(),
                disposition.as_str()
            )
        );
        assert_eq!(report, before);
    }

    report.review_disposition = ProcedureReviewDisposition::Approved;
    let before = report.clone();
    let consumed = require_approved_report(&report).unwrap();

    assert!(std::ptr::eq(consumed, &report));
    assert_eq!(consumed, &before);
}

#[test]
fn approved_report_round_trip_preserves_targets_dispatch_and_structural_validation() {
    let reports_dir = temp_path("approved-observability");
    let store = ProcedureReportStore::new(reports_dir.clone());
    let target = LocalizationTarget {
        path: "crates/deepseek-custom/src/procedure/report.rs".to_string(),
        symbol: Some("ProcedureReportStore".to_string()),
        evidence: "The store owns persisted review decisions.".to_string(),
    };
    let validation = OpenSpecValidation {
        command: vec![
            "openspec".to_string(),
            "validate".to_string(),
            "harden-procedure-localization".to_string(),
            "--strict".to_string(),
        ],
        exit_code: Some(0),
        stdout: "Change 'harden-procedure-localization' is valid".to_string(),
        stderr: String::new(),
    };
    let mut pending = awaiting_review_run();
    pending.validation = Some(validation.clone());
    pending.attempts = vec![
        LocalizationAttempt {
            number: 1,
            backend: "ollama".to_string(),
            model: "qwen2.5-coder:7b-instruct-q4_K_M".to_string(),
            disposition: ProcedureAttemptDisposition::Rejected,
            targets: Vec::new(),
            validation_error: Some("first structural response was rejected".to_string()),
        },
        LocalizationAttempt {
            number: 2,
            backend: "ollama".to_string(),
            model: "qwen2.5-coder:7b-instruct-q4_K_M".to_string(),
            disposition: ProcedureAttemptDisposition::Accepted,
            targets: vec![target.clone()],
            validation_error: None,
        },
    ];
    store.save(&pending).unwrap();

    store.approve(&pending.id).unwrap();
    let approved = store.load(&pending.id).unwrap();

    assert_eq!(
        approved.review_disposition,
        ProcedureReviewDisposition::Approved
    );
    assert_eq!(
        approved.terminal_disposition,
        Some(ProcedureTerminalDisposition::AwaitingReview)
    );
    assert_eq!(approved.validation, Some(validation));
    assert_eq!(approved.attempts.len(), 2);
    assert_eq!(
        approved
            .attempts
            .iter()
            .map(|attempt| attempt.disposition)
            .collect::<Vec<_>>(),
        vec![
            ProcedureAttemptDisposition::Rejected,
            ProcedureAttemptDisposition::Accepted,
        ]
    );
    assert_eq!(approved.attempts[1].backend, "ollama");
    assert_eq!(
        approved.attempts[1].model,
        "qwen2.5-coder:7b-instruct-q4_K_M"
    );
    assert_eq!(approved.attempts[1].targets, vec![target]);

    std::fs::remove_dir_all(reports_dir).ok();
}

#[test]
fn saved_verifier_evidence_round_trips_all_failure_details_through_json() {
    let reports_dir = temp_path("verification-round-trip");
    let store = ProcedureReportStore::new(reports_dir.clone());
    let report = completed_run();
    store.save(&report).unwrap();

    let output = BoundedVerifierOutput {
        text: "first diagnostic\n...[output truncated]...\nlast diagnostic".to_string(),
        first_edge: "first diagnostic".to_string(),
        last_edge: "last diagnostic".to_string(),
        truncated: true,
        bytes_seen: 12_345,
    };
    let command = VerifierCommandEvidence {
        command: "cargo test --workspace".to_string(),
        disposition: VerifierCommandDisposition::Failed,
        success: false,
        exit_code: Some(17),
        stdout: output.clone(),
        stderr: output.clone(),
        combined_output: output,
        duration_millis: 4_321,
        error: Some("test gate failed: assertion failed".to_string()),
    };
    let patch_output = BoundedVerifierOutput {
        text: "patch diagnostic".to_string(),
        first_edge: "patch diagnostic".to_string(),
        last_edge: "patch diagnostic".to_string(),
        truncated: false,
        bytes_seen: 16,
    };
    let patch_result = GitApplyResult {
        phase: GitApplyPhase::Check,
        command: "git apply --check".to_string(),
        disposition: GitApplyDisposition::Rejected,
        success: false,
        status_code: Some(1),
        stdout: BoundedVerifierOutput {
            text: String::new(),
            first_edge: String::new(),
            last_edge: String::new(),
            truncated: false,
            bytes_seen: 0,
        },
        stderr: patch_output.clone(),
        combined_output: patch_output,
        duration_millis: 27,
        error: None,
    };
    let verification = VerifierReport {
        patch_gates: vec![
            PatchGateEvidence {
                phase: GitApplyPhase::Check,
                command: "git apply --check".to_string(),
                disposition: PatchGateDisposition::Rejected,
                result: Some(patch_result),
            },
            PatchGateEvidence {
                phase: GitApplyPhase::Apply,
                command: "git apply".to_string(),
                disposition: PatchGateDisposition::NotRun {
                    blocked_by: GitApplyPhase::Check,
                },
                result: None,
            },
        ],
        gates: vec![
            VerifierGateEvidence {
                command: command.command.clone(),
                disposition: VerifierGateDisposition::Failed,
                result: Some(command),
            },
            VerifierGateEvidence {
                command: "cargo clippy --workspace".to_string(),
                disposition: VerifierGateDisposition::NotRun { blocked_by: 0 },
                result: None,
            },
        ],
        stopped_after_failure: true,
        first_failed_gate: Some(0),
        eligibility: CandidateEligibility::ineligible(
            CandidateIneligibility::VerifierCommandFailed {
                index: 0,
                command: "cargo test --workspace".to_string(),
                disposition: VerifierCommandDisposition::Failed,
            },
        ),
        terminal_disposition: Some(ProcedureTerminalDisposition::Failed {
            reason: "test gate failed".to_string(),
        }),
    };

    store.save_verification(&report.id, &verification).unwrap();

    let json = std::fs::read_to_string(store.report_path(&report.id)).unwrap();
    assert!(json.contains("cargo test --workspace"));
    assert!(json.contains("first diagnostic"));
    assert!(json.contains("last diagnostic"));
    assert!(json.contains("duration_millis"));
    assert!(json.contains("test gate failed: assertion failed"));
    assert!(json.contains("git apply --check"));
    assert!(json.contains("patch diagnostic"));
    assert!(json.contains("terminal_disposition"));

    let stored = store.load_with_fingerprints(&report.id).unwrap();
    let saved = stored.verification.expect("verification evidence is saved");
    assert_eq!(saved, verification);
    let patch = saved.patch_gates[0]
        .result
        .as_ref()
        .expect("rejected patch evidence is present");
    assert_eq!(patch.command, "git apply --check");
    assert_eq!(patch.status_code, Some(1));
    assert_eq!(patch.stderr.text, "patch diagnostic");
    assert_eq!(patch.duration_millis, 27);
    assert_eq!(patch.disposition, GitApplyDisposition::Rejected);
    assert_eq!(
        saved.patch_gates[1].disposition,
        PatchGateDisposition::NotRun {
            blocked_by: GitApplyPhase::Check,
        }
    );
    let failed = saved.gates[0]
        .result
        .as_ref()
        .expect("failed command evidence is present");
    assert_eq!(failed.command, "cargo test --workspace");
    assert_eq!(failed.exit_code, Some(17));
    assert_eq!(failed.stdout.first_edge, "first diagnostic");
    assert_eq!(failed.stdout.last_edge, "last diagnostic");
    assert!(failed.stdout.truncated);
    assert_eq!(failed.duration_millis, 4_321);
    assert_eq!(failed.disposition, VerifierCommandDisposition::Failed);
    assert_eq!(
        failed.error.as_deref(),
        Some("test gate failed: assertion failed")
    );
    assert_eq!(
        saved.gates[1].disposition,
        VerifierGateDisposition::NotRun { blocked_by: 0 }
    );
    assert!(saved.gates[1].result.is_none());

    std::fs::remove_dir_all(reports_dir).ok();
}
