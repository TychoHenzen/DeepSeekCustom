use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use deepseek_custom::agent::events::{AgentCommand, StreamEvent};
use deepseek_custom::config::settings::{
    ApiProvider, BackendConfig, ProcedureSettings, RepositoryIndexLimits, Settings,
};
use deepseek_custom::gui::DeepSeekGui;
use deepseek_custom::gui::agent_handles::AgentHandles;
use deepseek_custom::gui::procedure_tab::{ProcedureStatus, ProcedureTab};
use deepseek_custom::procedure::{
    LocalizationAttempt, LocalizationTarget, ProcedureAttemptDisposition, ProcedureProgress,
    ProcedureReportStore, ProcedureRun, ProcedureRunId, ProcedureScratchpad, ProcedureStage,
    ProcedureTask, ProcedureTerminalDisposition,
};
use tokio::sync::mpsc;

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "dsc-gui-procedure-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn write_change(root: &Path, id: &str, tasks: &str) {
    let change = root.join("openspec/changes").join(id);
    std::fs::create_dir_all(&change).unwrap();
    std::fs::write(change.join("tasks.md"), tasks).unwrap();
}

fn settings() -> Settings {
    let mut backends = HashMap::new();
    backends.insert(
        "deepseek".to_string(),
        BackendConfig::Api {
            provider: ApiProvider::DeepSeek,
            model: "deepseek-v4-pro".to_string(),
            base_url: None,
            api_key: None,
            models: None,
        },
    );
    backends.insert(
        "ollama-b".to_string(),
        BackendConfig::Api {
            provider: ApiProvider::Ollama,
            model: "qwen-b".to_string(),
            base_url: None,
            api_key: None,
            models: None,
        },
    );
    backends.insert(
        "ollama-a".to_string(),
        BackendConfig::Api {
            provider: ApiProvider::Ollama,
            model: "qwen-a".to_string(),
            base_url: None,
            api_key: None,
            models: None,
        },
    );
    backends.insert(
        "claude".to_string(),
        BackendConfig::ClaudeCli {
            model: "opus".to_string(),
            permission_mode: None,
            env: None,
            models: None,
        },
    );
    Settings {
        backends: Some(backends),
        procedure: Some(ProcedureSettings {
            localization_backend: Some("deepseek".to_string()),
            repository_index: RepositoryIndexLimits::default(),
        }),
        ..Settings::default()
    }
}

fn fixture_root(tag: &str) -> PathBuf {
    let root = temp_dir(tag);
    write_change(
        &root,
        "a-change",
        "## Tasks\n\n- [x] 1.0 Finished\n- [ ] 1.1 First pending\n- [ ] 1.2 Second pending\n",
    );
    write_change(&root, "empty-change", "## Tasks\n\n- [x] 1.0 Finished\n");
    root
}

fn gui_with_procedure(
    root: PathBuf,
) -> (
    DeepSeekGui,
    mpsc::UnboundedReceiver<AgentCommand>,
    mpsc::UnboundedReceiver<deepseek_custom::procedure::ProcedureCommand>,
    mpsc::UnboundedSender<ProcedureProgress>,
    Arc<AtomicBool>,
) {
    let (_event_tx, event_rx) = mpsc::unbounded_channel();
    let (agent_tx, agent_rx) = mpsc::unbounded_channel();
    let (procedure_tx, procedure_rx) = mpsc::unbounded_channel();
    let (progress_tx, progress_rx) = mpsc::unbounded_channel();
    let interrupt = Arc::new(AtomicBool::new(false));
    let gui = DeepSeekGui::new(
        event_rx,
        agent_tx,
        AgentHandles {
            interrupt: Arc::new(AtomicBool::new(false)),
            effort: Arc::new(AtomicU8::new(0)),
            voice_mode: Arc::new(AtomicBool::new(false)),
            context_budget: Arc::new(AtomicUsize::new(100_000)),
            model: Arc::new(Mutex::new("deepseek-v4-flash".to_string())),
            working_dir: Arc::new(Mutex::new(root.clone())),
            cascade_total: Arc::new(AtomicUsize::new(0)),
            cascade_escalated: Arc::new(AtomicUsize::new(0)),
            style_plain_language: Arc::new(AtomicBool::new(false)),
            style_target_grade: Arc::new(AtomicU8::new(8)),
        },
        settings(),
        root,
    )
    .with_procedure(procedure_tx, progress_rx, Arc::clone(&interrupt));
    (gui, agent_rx, procedure_rx, progress_tx, interrupt)
}

#[test]
fn tab_lists_pending_tasks_and_only_ollama_backends() {
    let root = fixture_root("selection");

    let tab = ProcedureTab::new(&settings(), &root);

    assert_eq!(tab.backend_names(), ["ollama-a", "ollama-b"]);
    assert_eq!(tab.changes().len(), 1);
    assert_eq!(tab.selected_change(), "a-change");
    assert_eq!(tab.selected_task(), "1.1");
    assert_eq!(
        tab.changes()[0]
            .tasks
            .iter()
            .map(|task| task.id.as_str())
            .collect::<Vec<_>>(),
        vec!["1.1", "1.2"]
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn run_sends_one_command_and_disables_a_second_start() {
    let root = fixture_root("command");
    let mut tab = ProcedureTab::new(&settings(), &root);
    let (command_tx, mut command_rx) = mpsc::unbounded_channel();
    let (_progress_tx, progress_rx) = mpsc::unbounded_channel();
    let interrupt = Arc::new(AtomicBool::new(true));
    tab.attach(command_tx, progress_rx, Arc::clone(&interrupt));

    tab.start_for_test();
    tab.start_for_test();

    let command = command_rx.try_recv().unwrap();
    assert_eq!(command.backend, "ollama-a");
    assert_eq!(command.request.change_id, "a-change");
    assert_eq!(command.request.task_id, "1.1");
    assert!(
        command_rx.try_recv().is_err(),
        "second start stays disabled"
    );
    assert!(
        !interrupt.load(Ordering::SeqCst),
        "new run clears stale stop"
    );
    assert!(tab.is_running());
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn completed_progress_loads_targets_dispatch_details_and_report_path() {
    let root = fixture_root("completed");
    let mut tab = ProcedureTab::new(&settings(), &root);
    let run_id = ProcedureRunId::new();
    let run = completed_run(run_id);
    ProcedureReportStore::for_project(&root).save(&run).unwrap();

    tab.handle_progress(ProcedureProgress::RunStarted {
        run_id,
        change_id: "a-change".to_string(),
        task_id: "1.1".to_string(),
    });
    tab.handle_progress(ProcedureProgress::AttemptStarted {
        run_id,
        number: 1,
        backend: "ollama-a".to_string(),
        model: "qwen-a".to_string(),
    });
    tab.handle_progress(ProcedureProgress::RunFinished {
        run_id,
        disposition: ProcedureTerminalDisposition::Succeeded,
    });

    assert_eq!(
        tab.status(),
        &ProcedureStatus::Finished(ProcedureTerminalDisposition::Succeeded)
    );
    let loaded = tab.latest_run().unwrap();
    assert_eq!(loaded.attempts.len(), 1);
    assert_eq!(loaded.attempts[0].backend, "ollama-a");
    assert_eq!(loaded.attempts[0].model, "qwen-a");
    assert_eq!(loaded.attempts[0].targets[0].evidence, "owns the runner");
    assert!(
        tab.latest_report_path()
            .unwrap()
            .ends_with(format!("{}.json", run_id.as_str()))
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn procedure_progress_stays_out_of_transcript_and_agent_history_path() {
    let root = fixture_root("event-isolation");
    let (mut gui, mut agent_rx, mut procedure_rx, progress_tx, _interrupt) =
        gui_with_procedure(root.clone());

    gui.procedure_mut_for_test().start_for_test();
    let command = procedure_rx.try_recv().unwrap();
    progress_tx
        .send(ProcedureProgress::RunStarted {
            run_id: ProcedureRunId::new(),
            change_id: command.request.change_id,
            task_id: command.request.task_id,
        })
        .unwrap();
    gui.drain_procedure_for_test();

    assert!(gui.transcript_for_test().blocks().is_empty());
    assert!(
        agent_rx.try_recv().is_err(),
        "procedure uses no AgentCommand, so MessageHistory cannot receive it"
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn session_reset_and_drop_interrupt_an_owned_procedure() {
    let root = fixture_root("lifecycle-stop");
    let (mut gui, _agent_rx, mut procedure_rx, _progress_tx, interrupt) =
        gui_with_procedure(root.clone());

    gui.procedure_mut_for_test().start_for_test();
    procedure_rx.try_recv().unwrap();
    gui.handle_stream_event(StreamEvent::SessionReset);
    assert!(interrupt.load(Ordering::SeqCst));
    assert!(gui.transcript_for_test().blocks().is_empty());

    interrupt.store(false, Ordering::SeqCst);
    gui.procedure_mut_for_test().start_for_test();
    drop(gui);
    assert!(interrupt.load(Ordering::SeqCst));
    std::fs::remove_dir_all(root).ok();
}

fn completed_run(id: ProcedureRunId) -> ProcedureRun {
    ProcedureRun {
        id,
        change_id: "a-change".to_string(),
        selected_task: ProcedureTask {
            id: "1.1".to_string(),
            text: "First pending".to_string(),
            covers: None,
        },
        spec_fingerprint: Some("spec".to_string()),
        repository_fingerprint: Some("repository".to_string()),
        validation: None,
        scratchpad: ProcedureScratchpad::default(),
        stage: ProcedureStage::Finished,
        attempts: vec![LocalizationAttempt {
            number: 1,
            backend: "ollama-a".to_string(),
            model: "qwen-a".to_string(),
            disposition: ProcedureAttemptDisposition::Accepted,
            targets: vec![LocalizationTarget {
                path: "src/procedure.rs".to_string(),
                symbol: Some("run".to_string()),
                evidence: "owns the runner".to_string(),
            }],
            validation_error: None,
        }],
        terminal_disposition: Some(ProcedureTerminalDisposition::Succeeded),
    }
}
