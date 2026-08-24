use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use deepseek_custom::agent::events::{AgentCommand, StreamEvent};
use deepseek_custom::config::settings::{
    ApiProvider, BackendConfig, ProcedureSettings, RepositoryIndexLimits, Settings,
};
use deepseek_custom::gui::DeepSeekGui;
use deepseek_custom::gui::agent_handles::AgentHandles;
use deepseek_custom::gui::procedure_tab::{ProcedureStatus, ProcedureTab, ProcedureViewState};
use deepseek_custom::procedure::{
    LocalizationAttempt, LocalizationTarget, OpenSpecValidation, ProcedureAttemptDisposition,
    ProcedureCommand, ProcedureProgress, ProcedureReportStore, ProcedureReviewDecision,
    ProcedureReviewDisposition, ProcedureRun, ProcedureRunId, ProcedureScratchpad, ProcedureStage,
    ProcedureTask, ProcedureTerminalDisposition, apply_review_decision,
};
use tokio::sync::mpsc;

const PROCEDURE_VISUAL_VERIFICATION_MANIFEST: &[(&str, &str)] = &[
    (
        "maintained GUI checklist",
        "docs/procedure-localization-verification.md",
    ),
    (
        "running screenshot",
        "docs/evidence/procedure-localization/running.png",
    ),
    (
        "awaiting-review screenshot",
        "docs/evidence/procedure-localization/awaiting-review.png",
    ),
    (
        "approved screenshot",
        "docs/evidence/procedure-localization/approved.png",
    ),
    (
        "rejected screenshot",
        "docs/evidence/procedure-localization/rejected.png",
    ),
    (
        "failed screenshot",
        "docs/evidence/procedure-localization/failed.png",
    ),
    (
        "interrupted screenshot",
        "docs/evidence/procedure-localization/interrupted.png",
    ),
];

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

fn attach_tab(tab: &mut ProcedureTab) -> mpsc::UnboundedReceiver<ProcedureCommand> {
    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let (_progress_tx, progress_rx) = mpsc::unbounded_channel();
    tab.attach(command_tx, progress_rx, Arc::new(AtomicBool::new(false)));
    command_rx
}

fn finish_review_command(
    root: &Path,
    tab: &mut ProcedureTab,
    command_rx: &mut mpsc::UnboundedReceiver<ProcedureCommand>,
) -> (ProcedureRunId, ProcedureReviewDecision) {
    let ProcedureCommand::Review { run_id, decision } = command_rx.try_recv().unwrap() else {
        panic!("review control must send a review command")
    };
    let (progress_tx, mut progress_rx) = mpsc::unbounded_channel();
    apply_review_decision(
        &ProcedureReportStore::for_project(root),
        run_id,
        decision,
        &progress_tx,
    );
    tab.handle_progress(progress_rx.try_recv().unwrap());
    (run_id, decision)
}

// covers: deepseek-custom/procedure-localization :: Localization is observable and non-mutating :: Procedure review states are visually inspectable
#[test]
fn procedure_visual_verification_manifest_requires_every_state_artifact() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repository = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("external test crate must remain under the repository's crates directory")
        .canonicalize()
        .expect("repository root must be readable");
    let mut problems = Vec::new();

    for (description, relative) in PROCEDURE_VISUAL_VERIFICATION_MANIFEST {
        let relative_path = Path::new(relative);
        let escapes_repository = relative_path.is_absolute()
            || relative_path.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            });
        if escapes_repository {
            problems.push(format!(
                "{description}: path must stay inside the repository: {relative}"
            ));
            continue;
        }

        let candidate = repository.join(relative_path);
        match candidate.canonicalize() {
            Ok(actual) if !actual.starts_with(&repository) => problems.push(format!(
                "{description}: resolved path escapes the repository: {relative}"
            )),
            Ok(actual) if !actual.is_file() => {
                problems.push(format!("{description}: is not a file: {relative}"));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                problems.push(format!("{description}: missing {relative}"));
            }
            Err(error) => problems.push(format!(
                "{description}: could not inspect {relative}: {error}"
            )),
        }
    }

    assert!(
        problems.is_empty(),
        "procedure visual verification manifest is incomplete:\n- {}",
        problems.join("\n- ")
    );
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

    let ProcedureCommand::Run {
        backend, request, ..
    } = command_rx.try_recv().unwrap()
    else {
        panic!("Run must send a localization command")
    };
    assert_eq!(backend, "ollama-a");
    assert_eq!(request.change_id, "a-change");
    assert_eq!(request.task_id, "1.1");
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
fn procedure_view_exposes_distinct_run_and_review_state_labels() {
    let root = fixture_root("state-labels");
    let mut tab = ProcedureTab::new(&settings(), &root);
    let run_id = ProcedureRunId::new();

    tab.handle_progress(ProcedureProgress::RunStarted {
        run_id,
        change_id: "a-change".to_string(),
        task_id: "1.1".to_string(),
    });
    assert_eq!(tab.view_state(), ProcedureViewState::Running);
    assert_eq!(tab.view_state().label(), "running");

    let mut pending = completed_run(run_id);
    pending.review_disposition = ProcedureReviewDisposition::Pending;
    pending.terminal_disposition = Some(ProcedureTerminalDisposition::AwaitingReview);
    ProcedureReportStore::for_project(&root)
        .save(&pending)
        .unwrap();
    tab.handle_progress(ProcedureProgress::RunFinished {
        run_id,
        disposition: ProcedureTerminalDisposition::AwaitingReview,
    });
    assert_eq!(tab.view_state(), ProcedureViewState::AwaitingReview);
    assert_eq!(tab.view_state().label(), "awaiting review");
    assert!(tab.review_actions_available_for_test());
    assert_eq!(
        tab.latest_run().unwrap().attempts[0].targets[0],
        LocalizationTarget {
            path: "src/procedure.rs".to_string(),
            symbol: Some("run".to_string()),
            evidence: "owns the runner".to_string(),
        }
    );

    let mut command_rx = attach_tab(&mut tab);
    tab.approve_for_test();
    finish_review_command(&root, &mut tab, &mut command_rx);
    assert_eq!(tab.view_state(), ProcedureViewState::Approved);
    assert_eq!(tab.view_state().label(), "approved");
    assert!(!tab.review_actions_available_for_test());

    let rejected_id = ProcedureRunId::new();
    let mut rejected = completed_run(rejected_id);
    rejected.review_disposition = ProcedureReviewDisposition::Pending;
    rejected.terminal_disposition = Some(ProcedureTerminalDisposition::AwaitingReview);
    ProcedureReportStore::for_project(&root)
        .save(&rejected)
        .unwrap();
    let mut tab = ProcedureTab::new(&settings(), &root);
    tab.handle_progress(ProcedureProgress::RunStarted {
        run_id: rejected_id,
        change_id: "a-change".to_string(),
        task_id: "1.1".to_string(),
    });
    tab.handle_progress(ProcedureProgress::RunFinished {
        run_id: rejected_id,
        disposition: ProcedureTerminalDisposition::AwaitingReview,
    });
    let mut command_rx = attach_tab(&mut tab);
    tab.reject_for_test();
    finish_review_command(&root, &mut tab, &mut command_rx);
    assert_eq!(tab.view_state(), ProcedureViewState::Rejected);
    assert_eq!(tab.view_state().label(), "rejected");
    assert!(!tab.review_actions_available_for_test());

    let failed_id = ProcedureRunId::new();
    let mut failed = completed_run(failed_id);
    failed.review_disposition = ProcedureReviewDisposition::Pending;
    failed.terminal_disposition = Some(ProcedureTerminalDisposition::Failed {
        reason: "exact fixture failure".to_string(),
    });
    ProcedureReportStore::for_project(&root)
        .save(&failed)
        .unwrap();
    let mut tab = ProcedureTab::new(&settings(), &root);
    tab.handle_progress(ProcedureProgress::RunStarted {
        run_id: failed_id,
        change_id: "a-change".to_string(),
        task_id: "1.1".to_string(),
    });
    tab.handle_progress(ProcedureProgress::RunFinished {
        run_id: failed_id,
        disposition: failed.terminal_disposition.clone().unwrap(),
    });
    assert_eq!(tab.view_state(), ProcedureViewState::Failed);
    assert_eq!(tab.view_state().label(), "failed");

    let interrupted_id = ProcedureRunId::new();
    let mut interrupted = completed_run(interrupted_id);
    interrupted.review_disposition = ProcedureReviewDisposition::Pending;
    interrupted.terminal_disposition = Some(ProcedureTerminalDisposition::Interrupted);
    ProcedureReportStore::for_project(&root)
        .save(&interrupted)
        .unwrap();
    let mut tab = ProcedureTab::new(&settings(), &root);
    tab.handle_progress(ProcedureProgress::RunStarted {
        run_id: interrupted_id,
        change_id: "a-change".to_string(),
        task_id: "1.1".to_string(),
    });
    tab.handle_progress(ProcedureProgress::RunFinished {
        run_id: interrupted_id,
        disposition: ProcedureTerminalDisposition::Interrupted,
    });
    assert_eq!(tab.view_state(), ProcedureViewState::Interrupted);
    assert_eq!(tab.view_state().label(), "interrupted");

    std::fs::remove_dir_all(root).ok();
}

#[test]
fn review_controls_are_visible_only_for_the_displayed_pending_run() {
    let root = fixture_root("review-controls");
    let store = ProcedureReportStore::for_project(&root);
    let visible_id = ProcedureRunId::new();
    let other_id = ProcedureRunId::new();
    let mut visible = completed_run(visible_id);
    visible.review_disposition = ProcedureReviewDisposition::Pending;
    visible.terminal_disposition = Some(ProcedureTerminalDisposition::AwaitingReview);
    let mut other = completed_run(other_id);
    other.review_disposition = ProcedureReviewDisposition::Pending;
    other.terminal_disposition = Some(ProcedureTerminalDisposition::AwaitingReview);
    store.save(&visible).unwrap();
    store.save(&other).unwrap();
    let mut tab = ProcedureTab::new(&settings(), &root);

    tab.handle_progress(ProcedureProgress::RunStarted {
        run_id: visible_id,
        change_id: "a-change".to_string(),
        task_id: "1.1".to_string(),
    });
    tab.handle_progress(ProcedureProgress::RunFinished {
        run_id: visible_id,
        disposition: ProcedureTerminalDisposition::AwaitingReview,
    });

    assert!(tab.review_actions_available_for_test());
    assert_eq!(tab.latest_run().unwrap().id, visible_id);
    assert_eq!(
        tab.latest_run().unwrap().attempts[0].targets,
        visible.attempts[0].targets
    );
    let mut command_rx = attach_tab(&mut tab);
    tab.approve_for_test();
    finish_review_command(&root, &mut tab, &mut command_rx);
    assert_eq!(
        tab.latest_run().unwrap().review_disposition,
        ProcedureReviewDisposition::Approved
    );
    assert!(!tab.review_actions_available_for_test());
    assert_eq!(
        store.load(&visible_id).unwrap().review_disposition,
        ProcedureReviewDisposition::Approved
    );
    assert_eq!(
        store.load(&other_id).unwrap().review_disposition,
        ProcedureReviewDisposition::Pending
    );

    std::fs::remove_dir_all(root).ok();
}

#[test]
fn review_failure_is_visible_and_keeps_the_pending_disposition() {
    let root = fixture_root("review-error");
    let store = ProcedureReportStore::for_project(&root);
    let run_id = ProcedureRunId::new();
    let mut pending = completed_run(run_id);
    pending.review_disposition = ProcedureReviewDisposition::Pending;
    pending.terminal_disposition = Some(ProcedureTerminalDisposition::AwaitingReview);
    store.save(&pending).unwrap();
    let mut tab = ProcedureTab::new(&settings(), &root);
    tab.handle_progress(ProcedureProgress::RunStarted {
        run_id,
        change_id: "a-change".to_string(),
        task_id: "1.1".to_string(),
    });
    tab.handle_progress(ProcedureProgress::RunFinished {
        run_id,
        disposition: ProcedureTerminalDisposition::AwaitingReview,
    });
    std::fs::remove_file(store.report_path(&run_id)).unwrap();

    let mut command_rx = attach_tab(&mut tab);
    tab.reject_for_test();
    finish_review_command(&root, &mut tab, &mut command_rx);

    assert_eq!(
        tab.latest_run().unwrap().review_disposition,
        ProcedureReviewDisposition::Pending
    );
    assert_eq!(tab.view_state(), ProcedureViewState::AwaitingReview);
    assert!(tab.review_actions_available_for_test());
    let error = tab.review_error().unwrap();
    assert!(error.contains(&run_id.as_str()));
    assert!(error.contains("could not load procedure run"));

    std::fs::remove_dir_all(root).ok();
}

// covers: deepseek-custom/procedure-localization :: Localization is observable and non-mutating :: Completed report is inspectable
#[test]
fn approved_structural_report_round_trip_populates_the_complete_procedure_view() {
    let root = fixture_root("approved-observability");
    let store = ProcedureReportStore::for_project(&root);
    let run_id = ProcedureRunId::new();
    let target = LocalizationTarget {
        path: "src/procedure.rs".to_string(),
        symbol: Some("run".to_string()),
        evidence: "The indexed symbol owns the localization runner.".to_string(),
    };
    let validation = OpenSpecValidation {
        command: vec![
            "openspec".to_string(),
            "validate".to_string(),
            "a-change".to_string(),
            "--strict".to_string(),
        ],
        exit_code: Some(0),
        stdout: "Change 'a-change' is valid".to_string(),
        stderr: String::new(),
    };
    let mut pending = completed_run(run_id);
    pending.validation = Some(validation.clone());
    pending.review_disposition = ProcedureReviewDisposition::Pending;
    pending.terminal_disposition = Some(ProcedureTerminalDisposition::AwaitingReview);
    pending.attempts = vec![
        LocalizationAttempt {
            number: 1,
            backend: "ollama-a".to_string(),
            model: "qwen-a".to_string(),
            disposition: ProcedureAttemptDisposition::Rejected,
            targets: Vec::new(),
            validation_error: Some("first result was not structurally valid".to_string()),
        },
        LocalizationAttempt {
            number: 2,
            backend: "ollama-a".to_string(),
            model: "qwen-a".to_string(),
            disposition: ProcedureAttemptDisposition::Accepted,
            targets: vec![target.clone()],
            validation_error: None,
        },
    ];
    store.save(&pending).unwrap();
    let mut review_tab = ProcedureTab::new(&settings(), &root);
    review_tab.handle_progress(ProcedureProgress::RunStarted {
        run_id,
        change_id: "a-change".to_string(),
        task_id: "1.1".to_string(),
    });
    review_tab.handle_progress(ProcedureProgress::RunFinished {
        run_id,
        disposition: ProcedureTerminalDisposition::AwaitingReview,
    });
    assert_eq!(review_tab.view_state(), ProcedureViewState::AwaitingReview);

    let mut command_rx = attach_tab(&mut review_tab);
    review_tab.approve_for_test();
    finish_review_command(&root, &mut review_tab, &mut command_rx);
    let persisted = store.load(&run_id).unwrap();
    let mut reloaded_tab = ProcedureTab::new(&settings(), &root);
    reloaded_tab.handle_progress(ProcedureProgress::RunStarted {
        run_id,
        change_id: "a-change".to_string(),
        task_id: "1.1".to_string(),
    });
    reloaded_tab.handle_progress(ProcedureProgress::RunFinished {
        run_id,
        disposition: ProcedureTerminalDisposition::AwaitingReview,
    });
    let visible = reloaded_tab.latest_run().unwrap();

    assert_eq!(reloaded_tab.view_state(), ProcedureViewState::Approved);
    assert_ne!(
        reloaded_tab.view_state(),
        ProcedureViewState::AwaitingReview
    );
    assert_eq!(visible, &persisted);
    assert_eq!(
        visible.review_disposition,
        ProcedureReviewDisposition::Approved
    );
    assert_eq!(visible.validation.as_ref(), Some(&validation));
    assert_eq!(visible.attempts.len(), 2);
    assert_eq!(
        visible
            .attempts
            .iter()
            .map(|attempt| attempt.disposition)
            .collect::<Vec<_>>(),
        vec![
            ProcedureAttemptDisposition::Rejected,
            ProcedureAttemptDisposition::Accepted,
        ]
    );
    assert_eq!(visible.attempts[1].backend, "ollama-a");
    assert_eq!(visible.attempts[1].model, "qwen-a");
    assert_eq!(visible.attempts[1].targets, vec![target]);
    assert!(!reloaded_tab.review_actions_available_for_test());

    std::fs::remove_dir_all(root).ok();
}

#[test]
fn procedure_progress_stays_out_of_transcript_and_agent_history_path() {
    let root = fixture_root("event-isolation");
    let (mut gui, mut agent_rx, mut procedure_rx, progress_tx, _interrupt) =
        gui_with_procedure(root.clone());

    gui.procedure_mut_for_test().start_for_test();
    let ProcedureCommand::Run {
        run_id, request, ..
    } = procedure_rx.try_recv().unwrap()
    else {
        panic!("start must send a run command")
    };
    progress_tx
        .send(ProcedureProgress::RunStarted {
            run_id,
            change_id: request.change_id,
            task_id: request.task_id,
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
fn stale_run_and_review_events_cannot_change_the_active_or_visible_run() {
    let root = fixture_root("stale-procedure-events");
    let store = ProcedureReportStore::for_project(&root);
    let active_id = ProcedureRunId::new();
    let stale_id = ProcedureRunId::new();
    let mut active = completed_run(active_id);
    active.review_disposition = ProcedureReviewDisposition::Pending;
    active.terminal_disposition = Some(ProcedureTerminalDisposition::AwaitingReview);
    let mut stale = completed_run(stale_id);
    stale.review_disposition = ProcedureReviewDisposition::Pending;
    stale.terminal_disposition = Some(ProcedureTerminalDisposition::AwaitingReview);
    store.save(&active).unwrap();
    store.save(&stale).unwrap();
    let mut tab = ProcedureTab::new(&settings(), &root);

    tab.handle_progress(ProcedureProgress::RunStarted {
        run_id: active_id,
        change_id: "a-change".to_string(),
        task_id: "1.1".to_string(),
    });
    tab.handle_progress(ProcedureProgress::AttemptStarted {
        run_id: stale_id,
        number: 2,
        backend: "stale-backend".to_string(),
        model: "stale-model".to_string(),
    });
    tab.handle_progress(ProcedureProgress::RunFinished {
        run_id: stale_id,
        disposition: ProcedureTerminalDisposition::AwaitingReview,
    });

    assert_eq!(
        tab.status(),
        &ProcedureStatus::Running {
            message: "Validating a-change task 1.1".to_string(),
        }
    );
    assert!(tab.latest_run().is_none());

    tab.handle_progress(ProcedureProgress::RunFinished {
        run_id: active_id,
        disposition: ProcedureTerminalDisposition::AwaitingReview,
    });
    store.approve(&stale_id).unwrap();
    tab.handle_progress(ProcedureProgress::ReviewSucceeded {
        run_id: stale_id,
        disposition: ProcedureReviewDisposition::Approved,
    });
    tab.handle_progress(ProcedureProgress::ReviewFailed {
        run_id: stale_id,
        disposition: ProcedureReviewDisposition::Rejected,
        error: "stale review failure".to_string(),
    });

    assert_eq!(tab.latest_run().unwrap().id, active_id);
    assert_eq!(
        tab.latest_run().unwrap().review_disposition,
        ProcedureReviewDisposition::Pending
    );
    assert_eq!(tab.view_state(), ProcedureViewState::AwaitingReview);
    assert!(tab.review_error().is_none());

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
        review_disposition: ProcedureReviewDisposition::Approved,
        terminal_disposition: Some(ProcedureTerminalDisposition::Succeeded),
    }
}
