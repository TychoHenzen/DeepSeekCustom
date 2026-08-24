use std::io;
use std::path::PathBuf;

use deepseek_custom::error::HarnessError;
use deepseek_custom::procedure::{
    LocalizationAttempt, LocalizationTarget, OpenSpecValidation, ProcedureApprovedReportError,
    ProcedureAttemptDisposition, ProcedureReportStore, ProcedureReviewDisposition, ProcedureRun,
    ProcedureRunId, ProcedureScratchpad, ProcedureStage, ProcedureTask,
    ProcedureTerminalDisposition, require_approved_report,
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
