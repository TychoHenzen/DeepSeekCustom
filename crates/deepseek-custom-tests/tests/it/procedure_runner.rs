use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use deepseek_custom::agent::history::MessageHistory;
use deepseek_custom::config::settings::RepositoryIndexLimits;
use deepseek_custom::procedure::{
    LocalizationDispatch, LocalizationDispatchError, LocalizationEnvelope, LocalizationTarget,
    ProcedureAttemptDisposition, ProcedureProgress, ProcedureReportStore, ProcedureRunRequest,
    ProcedureRunner, ProcedureScratchpad, ProcedureStage, ProcedureTerminalDisposition,
    RepositoryIndexEntry,
};

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "dsc-procedure-runner-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn write_fake_openspec(root: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let path = root.join("fake-openspec.cmd");
        std::fs::write(
            &path,
            "@echo off\r\necho %*\r\nif \"%2\"==\"fixture-change\" exit /b 0\r\necho exact validation failure for %2 1>&2\r\nexit /b 17\r\n",
        )
        .unwrap();
        path
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;

        let path = root.join("fake-openspec");
        std::fs::write(
            &path,
            "#!/bin/sh\nprintf '%s\\n' \"$*\"\nif [ \"$2\" = \"fixture-change\" ]; then exit 0; fi\nprintf 'exact validation failure for %s\\n' \"$2\" >&2\nexit 17\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        path
    }
}

fn write_fixture(root: &Path) -> PathBuf {
    let command = write_fake_openspec(root);
    let change = root.join("openspec/changes/fixture-change");
    let specs = change.join("specs/sample/capability");
    std::fs::create_dir_all(&specs).unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn target_symbol() {}\n").unwrap();
    std::fs::write(
        change.join("proposal.md"),
        "# Proposal\n\n## Why\n\nFind the implementation.\n\n## What Changes\n\n- Report one source target.\n",
    )
    .unwrap();
    std::fs::write(
        change.join("tasks.md"),
        "- [ ] 1.1 Localize the source target\n  <!-- covers: sample/capability :: Selected requirement :: Selected scenario -->\n",
    )
    .unwrap();
    std::fs::write(
        specs.join("spec.md"),
        "## Purpose\n\nFixture.\n\n## ADDED Requirements\n\n### Requirement: Selected requirement\nThe runner SHALL localize the selected source.\n\n#### Scenario: Selected scenario\n- **WHEN** the fixture runs\n- **THEN** the source target is reported\n",
    )
    .unwrap();
    command
}

fn request(change_id: &str) -> ProcedureRunRequest {
    ProcedureRunRequest {
        change_id: change_id.to_string(),
        task_id: "1.1".to_string(),
        scratchpad: ProcedureScratchpad {
            goals: vec!["find the source entry point".to_string()],
            ..ProcedureScratchpad::default()
        },
    }
}

fn limits() -> RepositoryIndexLimits {
    RepositoryIndexLimits {
        max_files: 100,
        max_total_bytes: 1_000_000,
    }
}

fn workspace_hash(root: &Path) -> u64 {
    fn collect(root: &Path, directory: &Path, files: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            let relative = path.strip_prefix(root).unwrap();
            if relative
                .components()
                .next()
                .is_some_and(|component| component.as_os_str() == ".deepseek")
            {
                continue;
            }
            if path.is_dir() {
                collect(root, &path, files);
            } else if path.is_file() {
                files.push(path);
            }
        }
    }

    let mut files = Vec::new();
    collect(root, root, &mut files);
    files.sort();
    let mut hash = 0xcbf29ce484222325_u64;
    for path in files {
        let relative = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        for byte in relative
            .bytes()
            .chain([0])
            .chain(std::fs::read(path).unwrap())
        {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    hash
}

#[derive(Clone)]
struct StubLocalizationDispatcher {
    responses: Arc<Mutex<VecDeque<Result<LocalizationEnvelope, LocalizationDispatchError>>>>,
    calls: Arc<AtomicUsize>,
    prompts: Arc<Mutex<Vec<String>>>,
    interrupt_on_dispatch: Option<Arc<AtomicBool>>,
}

impl StubLocalizationDispatcher {
    fn success(targets: Vec<LocalizationTarget>) -> Self {
        Self::script(vec![Ok(LocalizationEnvelope { targets })])
    }

    fn script(responses: Vec<Result<LocalizationEnvelope, LocalizationDispatchError>>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses.into())),
            calls: Arc::new(AtomicUsize::new(0)),
            prompts: Arc::new(Mutex::new(Vec::new())),
            interrupt_on_dispatch: None,
        }
    }

    fn interrupting(interrupt: Arc<AtomicBool>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(VecDeque::new())),
            calls: Arc::new(AtomicUsize::new(0)),
            prompts: Arc::new(Mutex::new(Vec::new())),
            interrupt_on_dispatch: Some(interrupt),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn prompts(&self) -> Vec<String> {
        self.prompts.lock().unwrap().clone()
    }
}

#[async_trait]
impl LocalizationDispatch for StubLocalizationDispatcher {
    fn backend_name(&self) -> &str {
        "stub-localizer"
    }

    fn model(&self) -> &str {
        "stub-model"
    }

    async fn dispatch_prompt(
        &self,
        prompt: String,
        _repository_index: &[RepositoryIndexEntry],
    ) -> Result<LocalizationEnvelope, LocalizationDispatchError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.prompts.lock().unwrap().push(prompt);
        if let Some(interrupt) = &self.interrupt_on_dispatch {
            interrupt.store(true, Ordering::SeqCst);
            std::future::pending::<()>().await;
            unreachable!("the runner must cancel an interrupted stub dispatch");
        }
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("stub script exhausted")
    }
}

#[tokio::test]
async fn stage_zero_and_one_finalize_a_saved_report_with_ordered_progress() {
    let root = temp_dir("success");
    let command = write_fixture(&root);
    let stub = StubLocalizationDispatcher::success(vec![LocalizationTarget {
        path: "src/lib.rs".to_string(),
        symbol: Some("target_symbol".to_string()),
        evidence: "The fixture source defines the selected entry point.".to_string(),
    }]);
    let store = ProcedureReportStore::for_project(&root);
    let interrupt = Arc::new(AtomicBool::new(false));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let runner = ProcedureRunner::new(
        deepseek_custom::procedure::OpenSpecInput::with_command(
            &root,
            command.display().to_string(),
        ),
        root.clone(),
        limits(),
        stub.clone(),
        ProcedureReportStore::for_project(&root),
        interrupt,
    )
    .with_progress(tx);

    let run = runner.run(request("fixture-change")).await.unwrap();
    let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();

    assert_eq!(stub.calls(), 1);
    assert_eq!(run.stage, ProcedureStage::Finished);
    assert!(run.validation.is_some());
    assert!(
        run.spec_fingerprint
            .as_deref()
            .unwrap()
            .starts_with("fnv1a64:")
    );
    assert!(
        run.repository_fingerprint
            .as_deref()
            .unwrap()
            .starts_with("fnv1a64:")
    );
    assert_eq!(run.attempts.len(), 1);
    assert_eq!(
        run.attempts[0].disposition,
        ProcedureAttemptDisposition::Accepted
    );
    assert_eq!(run.scratchpad.files, vec!["src/lib.rs"]);
    assert_eq!(
        run.terminal_disposition,
        Some(ProcedureTerminalDisposition::Succeeded)
    );
    assert_eq!(store.load(&run.id).unwrap(), run);
    assert!(matches!(events[0], ProcedureProgress::RunStarted { .. }));
    assert!(matches!(
        events[1],
        ProcedureProgress::StageStarted {
            stage: ProcedureStage::SpecValidation,
            ..
        }
    ));
    assert!(matches!(
        events[2],
        ProcedureProgress::StageCompleted {
            stage: ProcedureStage::SpecValidation,
            ..
        }
    ));
    assert!(matches!(
        events[3],
        ProcedureProgress::StageStarted {
            stage: ProcedureStage::Localization,
            ..
        }
    ));
    assert!(matches!(
        events[4],
        ProcedureProgress::AttemptStarted { .. }
    ));
    assert!(matches!(
        events[5],
        ProcedureProgress::AttemptAccepted { .. }
    ));
    assert!(matches!(
        events[6],
        ProcedureProgress::StageCompleted {
            stage: ProcedureStage::Localization,
            ..
        }
    ));
    assert!(matches!(events[7], ProcedureProgress::RunFinished { .. }));
    assert_eq!(events.len(), 8);
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn failed_stage_zero_saves_exact_failure_without_model_dispatch() {
    let root = temp_dir("stage-zero-failure");
    let command = write_fake_openspec(&root);
    let stub = StubLocalizationDispatcher::success(Vec::new());
    let runner = ProcedureRunner::new(
        deepseek_custom::procedure::OpenSpecInput::with_command(
            &root,
            command.display().to_string(),
        ),
        root.clone(),
        limits(),
        stub.clone(),
        ProcedureReportStore::for_project(&root),
        Arc::new(AtomicBool::new(false)),
    );

    let run = runner.run(request("invalid-change")).await.unwrap();

    assert_eq!(stub.calls(), 0);
    let Some(ProcedureTerminalDisposition::Failed { reason }) = &run.terminal_disposition else {
        panic!("expected failed run")
    };
    assert!(reason.contains("exact validation failure for invalid-change"));
    assert!(run.attempts.is_empty());
    assert_eq!(
        ProcedureReportStore::for_project(&root)
            .load(&run.id)
            .unwrap(),
        run
    );
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn invalid_first_result_is_retried_with_exact_error_and_valid_second_result_wins() {
    let root = temp_dir("repair");
    let command = write_fixture(&root);
    let invalid_target = LocalizationTarget {
        path: "src/invented.rs".to_string(),
        symbol: None,
        evidence: "This target is intentionally absent.".to_string(),
    };
    let valid_target = LocalizationTarget {
        path: "src/lib.rs".to_string(),
        symbol: Some("target_symbol".to_string()),
        evidence: "The indexed source defines the target.".to_string(),
    };
    let stub = StubLocalizationDispatcher::script(vec![
        Ok(LocalizationEnvelope {
            targets: vec![invalid_target],
        }),
        Ok(LocalizationEnvelope {
            targets: vec![valid_target.clone()],
        }),
    ]);
    let runner = ProcedureRunner::new(
        deepseek_custom::procedure::OpenSpecInput::with_command(
            &root,
            command.display().to_string(),
        ),
        root.clone(),
        limits(),
        stub.clone(),
        ProcedureReportStore::for_project(&root),
        Arc::new(AtomicBool::new(false)),
    );

    let run = runner.run(request("fixture-change")).await.unwrap();
    let exact_error = "localization target validation failed:\n- target[0] path=\"src/invented.rs\": path is not present in the repository index\n";

    assert_eq!(stub.calls(), 2);
    assert_eq!(run.attempts.len(), 2);
    assert_eq!(
        run.attempts[0].validation_error.as_deref(),
        Some(exact_error)
    );
    assert_eq!(
        run.attempts[1].disposition,
        ProcedureAttemptDisposition::Accepted
    );
    assert_eq!(run.attempts[1].targets, vec![valid_target]);
    assert_eq!(run.scratchpad.last_error.as_deref(), Some(exact_error));
    assert_eq!(
        run.terminal_disposition,
        Some(ProcedureTerminalDisposition::Succeeded)
    );
    assert_eq!(
        ProcedureReportStore::for_project(&root)
            .load(&run.id)
            .unwrap(),
        run
    );

    let prompts = stub.prompts();
    assert_eq!(prompts.len(), 2);
    let first: serde_json::Value =
        serde_json::from_str(prompts[0].split_once("\n\n").unwrap().1).unwrap();
    let second: serde_json::Value =
        serde_json::from_str(prompts[1].split_once("\n\n").unwrap().1).unwrap();
    assert_eq!(first["scratchpad"]["last_error"], serde_json::Value::Null);
    assert_eq!(second["scratchpad"]["last_error"], exact_error);
    assert!(second.get("history").is_none());
    assert!(second.get("messages").is_none());
    assert!(!prompts[1].contains("This target is intentionally absent."));
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn malformed_final_json_is_the_only_dispatch_error_that_gets_a_repair_attempt() {
    let root = temp_dir("malformed-repair");
    let command = write_fixture(&root);
    let stub = StubLocalizationDispatcher::script(vec![
        Err(LocalizationDispatchError::InvalidEnvelope {
            reason: "expected value at line 1 column 1".to_string(),
        }),
        Ok(LocalizationEnvelope {
            targets: Vec::new(),
        }),
    ]);
    let runner = ProcedureRunner::new(
        deepseek_custom::procedure::OpenSpecInput::with_command(
            &root,
            command.display().to_string(),
        ),
        root.clone(),
        limits(),
        stub.clone(),
        ProcedureReportStore::for_project(&root),
        Arc::new(AtomicBool::new(false)),
    );

    let run = runner.run(request("fixture-change")).await.unwrap();

    assert_eq!(stub.calls(), 2);
    assert_eq!(run.attempts.len(), 2);
    assert_eq!(
        run.terminal_disposition,
        Some(ProcedureTerminalDisposition::Succeeded)
    );
    assert_eq!(
        run.attempts[0].validation_error.as_deref(),
        Some(
            "localization response final content is not a valid LocalizationEnvelope: expected value at line 1 column 1"
        )
    );
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn transport_failure_does_not_consume_the_procedure_repair_attempt() {
    let root = temp_dir("transport-failure");
    let command = write_fixture(&root);
    let stub = StubLocalizationDispatcher::script(vec![
        Err(LocalizationDispatchError::Request {
            backend: "stub-localizer".to_string(),
            reason: "connection closed".to_string(),
        }),
        panic_if_dispatched(),
    ]);
    let runner = ProcedureRunner::new(
        deepseek_custom::procedure::OpenSpecInput::with_command(
            &root,
            command.display().to_string(),
        ),
        root.clone(),
        limits(),
        stub.clone(),
        ProcedureReportStore::for_project(&root),
        Arc::new(AtomicBool::new(false)),
    );

    let run = runner.run(request("fixture-change")).await.unwrap();

    assert_eq!(stub.calls(), 1);
    assert_eq!(run.attempts.len(), 1);
    assert_eq!(
        run.terminal_disposition,
        Some(ProcedureTerminalDisposition::Failed {
            reason:
                "localization request through backend \"stub-localizer\" failed: connection closed"
                    .to_string(),
        })
    );
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn two_invalid_results_save_both_errors_and_never_make_a_third_call() {
    let root = temp_dir("repair-exhausted");
    let command = write_fixture(&root);
    let stub = StubLocalizationDispatcher::script(vec![
        Err(LocalizationDispatchError::InvalidEnvelope {
            reason: "first malformed response".to_string(),
        }),
        Ok(LocalizationEnvelope {
            targets: vec![LocalizationTarget {
                path: "src/lib.rs".to_string(),
                symbol: Some("invented_symbol".to_string()),
                evidence: "The symbol is intentionally absent.".to_string(),
            }],
        }),
        panic_if_dispatched(),
    ]);
    let runner = ProcedureRunner::new(
        deepseek_custom::procedure::OpenSpecInput::with_command(
            &root,
            command.display().to_string(),
        ),
        root.clone(),
        limits(),
        stub.clone(),
        ProcedureReportStore::for_project(&root),
        Arc::new(AtomicBool::new(false)),
    );

    let run = runner.run(request("fixture-change")).await.unwrap();
    let first_error = "localization response final content is not a valid LocalizationEnvelope: first malformed response";
    let second_error = "localization target validation failed:\n- target[0] path=\"src/lib.rs\" symbol=\"invented_symbol\": symbol is not present under the indexed path\n";

    assert_eq!(stub.calls(), 2);
    assert_eq!(stub.prompts().len(), 2);
    assert_eq!(run.attempts.len(), 2);
    assert_eq!(
        run.attempts[0].validation_error.as_deref(),
        Some(first_error)
    );
    assert_eq!(
        run.attempts[1].validation_error.as_deref(),
        Some(second_error)
    );
    assert!(
        run.attempts
            .iter()
            .all(|attempt| attempt.disposition == ProcedureAttemptDisposition::Rejected)
    );
    assert_eq!(
        run.terminal_disposition,
        Some(ProcedureTerminalDisposition::Failed {
            reason: second_error.to_string(),
        })
    );
    assert_eq!(
        ProcedureReportStore::for_project(&root)
            .load(&run.id)
            .unwrap(),
        run
    );
    std::fs::remove_dir_all(root).ok();
}

fn panic_if_dispatched() -> Result<LocalizationEnvelope, LocalizationDispatchError> {
    Err(LocalizationDispatchError::Request {
        backend: "third-call-sentinel".to_string(),
        reason: "a third call was made".to_string(),
    })
}

#[tokio::test]
async fn success_failure_and_interruption_leave_the_fixture_workspace_unchanged() {
    let history = MessageHistory::new("chat history sentinel".to_string());

    let success_root = temp_dir("hash-success");
    let success_command = write_fixture(&success_root);
    let success_before = workspace_hash(&success_root);
    let success_stub = StubLocalizationDispatcher::success(vec![LocalizationTarget {
        path: "src/lib.rs".to_string(),
        symbol: Some("target_symbol".to_string()),
        evidence: "The indexed fixture defines this symbol.".to_string(),
    }]);
    let (success_tx, mut success_rx) = tokio::sync::mpsc::unbounded_channel();
    let success_runner = ProcedureRunner::new(
        deepseek_custom::procedure::OpenSpecInput::with_command(
            &success_root,
            success_command.display().to_string(),
        ),
        success_root.clone(),
        limits(),
        success_stub,
        ProcedureReportStore::for_project(&success_root),
        Arc::new(AtomicBool::new(false)),
    )
    .with_progress(success_tx);
    let success = success_runner.run(request("fixture-change")).await.unwrap();
    let success_events: Vec<_> = std::iter::from_fn(|| success_rx.try_recv().ok()).collect();
    assert_eq!(workspace_hash(&success_root), success_before);
    assert_eq!(
        success.terminal_disposition,
        Some(ProcedureTerminalDisposition::Succeeded)
    );
    assert!(matches!(
        success_events.last(),
        Some(ProcedureProgress::RunFinished {
            disposition: ProcedureTerminalDisposition::Succeeded,
            ..
        })
    ));
    assert_eq!(
        ProcedureReportStore::for_project(&success_root)
            .load(&success.id)
            .unwrap(),
        success
    );

    let failure_root = temp_dir("hash-failure");
    let failure_command = write_fixture(&failure_root);
    let failure_before = workspace_hash(&failure_root);
    let failure_stub = StubLocalizationDispatcher::script(vec![
        Err(LocalizationDispatchError::InvalidEnvelope {
            reason: "first invalid result".to_string(),
        }),
        Err(LocalizationDispatchError::InvalidEnvelope {
            reason: "second invalid result".to_string(),
        }),
    ]);
    let (failure_tx, mut failure_rx) = tokio::sync::mpsc::unbounded_channel();
    let failure_runner = ProcedureRunner::new(
        deepseek_custom::procedure::OpenSpecInput::with_command(
            &failure_root,
            failure_command.display().to_string(),
        ),
        failure_root.clone(),
        limits(),
        failure_stub.clone(),
        ProcedureReportStore::for_project(&failure_root),
        Arc::new(AtomicBool::new(false)),
    )
    .with_progress(failure_tx);
    let failure = failure_runner.run(request("fixture-change")).await.unwrap();
    let failure_events: Vec<_> = std::iter::from_fn(|| failure_rx.try_recv().ok()).collect();
    assert_eq!(workspace_hash(&failure_root), failure_before);
    assert_eq!(failure_stub.calls(), 2);
    assert!(matches!(
        failure.terminal_disposition,
        Some(ProcedureTerminalDisposition::Failed { .. })
    ));
    assert!(matches!(
        failure_events.last(),
        Some(ProcedureProgress::RunFinished {
            disposition: ProcedureTerminalDisposition::Failed { .. },
            ..
        })
    ));
    assert_eq!(
        ProcedureReportStore::for_project(&failure_root)
            .load(&failure.id)
            .unwrap(),
        failure
    );

    let interrupted_root = temp_dir("hash-interrupted");
    let interrupted_command = write_fixture(&interrupted_root);
    let interrupted_before = workspace_hash(&interrupted_root);
    let interrupt = Arc::new(AtomicBool::new(false));
    let interrupted_stub = StubLocalizationDispatcher::interrupting(Arc::clone(&interrupt));
    let (interrupted_tx, mut interrupted_rx) = tokio::sync::mpsc::unbounded_channel();
    let interrupted_runner = ProcedureRunner::new(
        deepseek_custom::procedure::OpenSpecInput::with_command(
            &interrupted_root,
            interrupted_command.display().to_string(),
        ),
        interrupted_root.clone(),
        limits(),
        interrupted_stub.clone(),
        ProcedureReportStore::for_project(&interrupted_root),
        interrupt,
    )
    .with_progress(interrupted_tx);
    let interrupted = interrupted_runner
        .run(request("fixture-change"))
        .await
        .unwrap();
    let interrupted_events: Vec<_> =
        std::iter::from_fn(|| interrupted_rx.try_recv().ok()).collect();
    assert_eq!(workspace_hash(&interrupted_root), interrupted_before);
    assert_eq!(interrupted_stub.calls(), 1);
    assert_eq!(
        interrupted.terminal_disposition,
        Some(ProcedureTerminalDisposition::Interrupted)
    );
    assert_eq!(
        interrupted.attempts[0].disposition,
        ProcedureAttemptDisposition::Interrupted
    );
    assert!(matches!(
        interrupted_events.last(),
        Some(ProcedureProgress::RunFinished {
            disposition: ProcedureTerminalDisposition::Interrupted,
            ..
        })
    ));
    assert_eq!(
        ProcedureReportStore::for_project(&interrupted_root)
            .load(&interrupted.id)
            .unwrap(),
        interrupted
    );

    assert_eq!(history.len(), 0);
    std::fs::remove_dir_all(success_root).ok();
    std::fs::remove_dir_all(failure_root).ok();
    std::fs::remove_dir_all(interrupted_root).ok();
}
