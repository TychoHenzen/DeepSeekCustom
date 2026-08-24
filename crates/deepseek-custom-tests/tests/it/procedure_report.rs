use std::io;
use std::path::PathBuf;

use deepseek_custom::error::HarnessError;
use deepseek_custom::procedure::{
    LocalizationAttempt, LocalizationTarget, ProcedureAttemptDisposition, ProcedureReportStore,
    ProcedureRun, ProcedureRunId, ProcedureScratchpad, ProcedureStage, ProcedureTask,
    ProcedureTerminalDisposition,
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
