//! One real read-only localization run against the configured Ollama backend.
//!
//! Usage from the repository root:
//! `cargo run -p deepseek-custom --example procedure_localization_smoke`
//! `cargo run -p deepseek-custom --example procedure_localization_smoke -- controlled-review approve`
//! `cargo run -p deepseek-custom --example procedure_localization_smoke -- controlled-review reject`

use std::error::Error;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use deepseek_custom::config::settings::Settings;
use deepseek_custom::procedure::{
    LocalizationAttempt, LocalizationDispatcher, LocalizationTarget, OpenSpecInput,
    ProcedureAttemptDisposition, ProcedureReportStore, ProcedureReviewDisposition, ProcedureRun,
    ProcedureRunId, ProcedureRunRequest, ProcedureRunner, ProcedureScratchpad, ProcedureStage,
    ProcedureTask, ProcedureTerminalDisposition, build_repository_index,
    validate_localization_targets,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt::init();
    let project_root = std::env::current_dir()?;

    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if let [command, decision] = arguments.as_slice()
        && command == "controlled-review"
    {
        return run_controlled_review(&project_root, decision);
    }
    if !arguments.is_empty() {
        return Err(
            "usage: procedure_localization_smoke [controlled-review <approve|reject>]".into(),
        );
    }

    run_live_smoke(&project_root).await
}

async fn run_live_smoke(project_root: &std::path::Path) -> Result<(), Box<dyn Error>> {
    let mut settings = Settings::load(project_root)?;
    settings.procedure_mut().localization_backend = Some("ollama".to_string());
    let limits = settings.procedure_mut().repository_index.clone();
    let working_dir = configured_working_dir(&settings, project_root);
    let dispatcher = LocalizationDispatcher::from_settings(&settings, project_root)?;
    let reports = ProcedureReportStore::for_project(project_root);
    let runner = ProcedureRunner::new(
        OpenSpecInput::new(project_root),
        working_dir,
        limits,
        dispatcher,
        reports,
        Arc::new(AtomicBool::new(false)),
    );

    let run = runner
        .run(ProcedureRunRequest {
            change_id: "implement-hemisphere-model".to_string(),
            task_id: "1.1".to_string(),
            scratchpad: ProcedureScratchpad::default(),
        })
        .await?;
    let report_path = ProcedureReportStore::for_project(project_root).report_path(&run.id);
    println!("REPORT_PATH={}", report_path.display());
    println!("{}", serde_json::to_string_pretty(&run)?);
    Ok(())
}

fn run_controlled_review(
    project_root: &std::path::Path,
    decision: &str,
) -> Result<(), Box<dyn Error>> {
    if !matches!(decision, "approve" | "reject") {
        return Err("controlled review decision must be approve or reject".into());
    }

    let mut settings = Settings::load(project_root)?;
    let target = LocalizationTarget {
        path: "crates/deepseek-custom/src/config/settings.rs".to_string(),
        symbol: None,
        evidence: "Task 1.1 names this file as the owner of HemisphereSettings.".to_string(),
    };
    let limits = settings.procedure_mut().repository_index.clone();
    let index = build_repository_index(project_root, &limits)?;
    let targets = validate_localization_targets(vec![target], &index)?;
    let report = ProcedureRun {
        id: ProcedureRunId::new(),
        change_id: "implement-hemisphere-model".to_string(),
        selected_task: ProcedureTask {
            id: "1.1".to_string(),
            text: "Add a HemisphereSettings block to crates/deepseek-custom/src/config/settings.rs with enabled, advisor_backend, view_budget_chars, keep_verbatim_turns, and max_reply_tokens, every field optional with skip_serializing_if = \"Option::is_none\", and defaults from design.md's table."
                .to_string(),
            covers: None,
        },
        spec_fingerprint: None,
        repository_fingerprint: None,
        validation: None,
        scratchpad: ProcedureScratchpad::default(),
        stage: ProcedureStage::Finished,
        attempts: vec![LocalizationAttempt {
            number: 1,
            backend: "controlled-review".to_string(),
            model: "not-dispatched".to_string(),
            disposition: ProcedureAttemptDisposition::Accepted,
            targets,
            validation_error: None,
        }],
        review_disposition: ProcedureReviewDisposition::Pending,
        terminal_disposition: Some(ProcedureTerminalDisposition::AwaitingReview),
    };
    let store = ProcedureReportStore::for_project(project_root);
    store.save(&report)?;
    let reviewed = match decision {
        "approve" => store.approve(&report.id)?,
        "reject" => store.reject(&report.id)?,
        _ => unreachable!("controlled review decision was validated before report creation"),
    };

    println!("CONTROLLED_REVIEW={decision}");
    println!("MODEL_DISPATCHED=false");
    println!("REPORT_PATH={}", store.report_path(&reviewed.id).display());
    println!("{}", serde_json::to_string_pretty(&reviewed)?);
    Ok(())
}

fn configured_working_dir(settings: &Settings, project_root: &std::path::Path) -> PathBuf {
    settings
        .working_dir()
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .unwrap_or_else(|| project_root.to_path_buf())
}
