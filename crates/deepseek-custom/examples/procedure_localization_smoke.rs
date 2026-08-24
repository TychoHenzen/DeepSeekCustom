//! One real read-only localization run against the configured Ollama backend.
//!
//! Usage from the repository root:
//! `cargo run -p deepseek-custom --example procedure_localization_smoke`

use std::error::Error;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use deepseek_custom::config::settings::Settings;
use deepseek_custom::procedure::{
    LocalizationDispatcher, OpenSpecInput, ProcedureReportStore, ProcedureRunRequest,
    ProcedureRunner, ProcedureScratchpad,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt::init();
    let project_root = std::env::current_dir()?;
    let mut settings = Settings::load(&project_root)?;
    settings.procedure_mut().localization_backend = Some("ollama".to_string());
    let limits = settings.procedure_mut().repository_index.clone();
    let working_dir = configured_working_dir(&settings, &project_root);
    let dispatcher = LocalizationDispatcher::from_settings(&settings, &project_root)?;
    let reports = ProcedureReportStore::for_project(&project_root);
    let runner = ProcedureRunner::new(
        OpenSpecInput::new(&project_root),
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
    let report_path = ProcedureReportStore::for_project(&project_root).report_path(&run.id);
    println!("REPORT_PATH={}", report_path.display());
    println!("{}", serde_json::to_string_pretty(&run)?);
    Ok(())
}

fn configured_working_dir(settings: &Settings, project_root: &std::path::Path) -> PathBuf {
    settings
        .working_dir()
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .unwrap_or_else(|| project_root.to_path_buf())
}
