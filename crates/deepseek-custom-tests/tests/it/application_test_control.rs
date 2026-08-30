use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use deepseek_custom::application::test_control::{
    ActiveTestSlot, CargoTestExecutor, RetainedTestResult, TEST_DISCOVERY_DIAGNOSTIC_LIMIT_BYTES,
    TestClock, TestControlSnapshot, TestCounts, TestDiscoveryState, TestExecution,
    TestExecutionError, TestExecutor, TestIdentity, TestInvocation, TestOutcome, TestOutputChunk,
    TestOutputStream, TestProcessExit, TestResultStore, TestRunCoordinator, TestRunRequest,
    TestRunRequestError, TestScope, classify_test_outcome, full_workspace_test_invocation,
    integration_test_discovery_invocation,
};

struct FixedClock(u64);

impl TestClock for FixedClock {
    fn now_ms(&self) -> u64 {
        self.0
    }
}

fn retained_result(number: u64) -> RetainedTestResult {
    RetainedTestResult {
        run_id: format!("run-{number}"),
        identity: TestIdentity {
            name: format!("application_test_control::case_{number}"),
            scope: TestScope::Exact {
                module: "application_test_control".into(),
                test: format!("application_test_control::case_{number}"),
            },
        },
        command: vec!["cargo".into(), "test".into(), format!("case_{number}")],
        working_dir: PathBuf::from("fixed-project-root"),
        started_at_ms: number * 100,
        duration_ms: number * 10,
        outcome: if number.is_multiple_of(2) {
            TestOutcome::Passed
        } else {
            TestOutcome::Failed
        },
        counts: TestCounts {
            passed: u64::from(number.is_multiple_of(2)),
            failed: u64::from(!number.is_multiple_of(2)),
            ignored: 0,
            filtered: number,
        },
        exit_code: Some(if number.is_multiple_of(2) { 0 } else { 101 }),
        failed_tests: Vec::new(),
        output: format!("diagnostic output {number}\n"),
        omitted_output_bytes: 0,
    }
}

struct ScriptedExecutor {
    chunks: Vec<TestOutputChunk>,
    exit: TestProcessExit,
}

impl TestExecutor for ScriptedExecutor {
    fn start(
        &self,
        _invocation: &TestInvocation,
    ) -> Result<Box<dyn TestExecution>, TestExecutionError> {
        Ok(Box::new(ScriptedExecution {
            chunks: self.chunks.clone().into_iter(),
            exit: self.exit,
        }))
    }
}

struct ScriptedExecution {
    chunks: std::vec::IntoIter<TestOutputChunk>,
    exit: TestProcessExit,
}

struct RunningExecutor;

impl TestExecutor for RunningExecutor {
    fn start(
        &self,
        _invocation: &TestInvocation,
    ) -> Result<Box<dyn TestExecution>, TestExecutionError> {
        Ok(Box::new(RunningExecution {
            output: Some(TestOutputChunk {
                sequence: 0,
                stream: TestOutputStream::Stdout,
                text: "still running\n".into(),
            }),
        }))
    }
}

struct RunningExecution {
    output: Option<TestOutputChunk>,
}

impl TestExecution for RunningExecution {
    fn next_output(&mut self) -> Result<Option<TestOutputChunk>, TestExecutionError> {
        Ok(self.output.take())
    }

    fn try_wait(&mut self) -> Result<Option<TestProcessExit>, TestExecutionError> {
        Ok(None)
    }

    fn cancel_and_wait(&mut self) -> Result<TestProcessExit, TestExecutionError> {
        Ok(TestProcessExit { exit_code: None })
    }
}

impl TestExecution for ScriptedExecution {
    fn next_output(&mut self) -> Result<Option<TestOutputChunk>, TestExecutionError> {
        Ok(self.chunks.next())
    }

    fn try_wait(&mut self) -> Result<Option<TestProcessExit>, TestExecutionError> {
        Ok(self.chunks.as_slice().is_empty().then_some(self.exit))
    }

    fn cancel_and_wait(&mut self) -> Result<TestProcessExit, TestExecutionError> {
        self.chunks = Vec::new().into_iter();
        Ok(TestProcessExit { exit_code: None })
    }
}

#[test]
fn test_control_contracts_keep_scope_output_and_time_deterministic() {
    let identity = TestIdentity {
        name: "application_test_control::exact".into(),
        scope: TestScope::Exact {
            module: "application_test_control".into(),
            test: "exact".into(),
        },
    };
    let request = TestRunRequest {
        identity: identity.clone(),
        catalogue_revision: 1,
    };
    let invocation = TestInvocation {
        program: "cargo".into(),
        args: vec!["test".into(), "--exact".into(), identity.name.clone()],
        working_dir: PathBuf::from("project"),
    };
    let executor = ScriptedExecutor {
        chunks: vec![
            TestOutputChunk {
                sequence: 0,
                stream: TestOutputStream::Stdout,
                text: "running 1 test\n".into(),
            },
            TestOutputChunk {
                sequence: 1,
                stream: TestOutputStream::Stderr,
                text: "diagnostic\n".into(),
            },
        ],
        exit: TestProcessExit { exit_code: Some(0) },
    };
    let mut execution = executor.start(&invocation).unwrap();
    let mut observed = Vec::new();
    while let Some(chunk) = execution.next_output().unwrap() {
        observed.push(chunk);
    }
    let exit = execution.try_wait().unwrap().unwrap();
    let clock = FixedClock(1_725_000_000_000);

    assert_eq!(request.identity, identity);
    assert_eq!(observed[0].sequence, 0);
    assert_eq!(observed[1].stream, TestOutputStream::Stderr);
    assert_eq!(exit.exit_code, Some(0));
    assert_eq!(clock.now_ms(), 1_725_000_000_000);

    let mut cancelled = executor.start(&invocation).unwrap();
    assert_eq!(cancelled.cancel_and_wait().unwrap().exit_code, None);
    assert_eq!(cancelled.next_output().unwrap(), None);
}

#[test]
fn active_and_retained_results_have_distinct_non_terminal_and_terminal_shapes() {
    let identity = TestIdentity {
        name: "workspace".into(),
        scope: TestScope::FullWorkspace,
    };
    let active = ActiveTestSlot {
        run_id: "run-1".into(),
        identity: identity.clone(),
        command: vec!["cargo".into(), "test".into(), "--workspace".into()],
        working_dir: PathBuf::from("project"),
        started_at_ms: 10,
        elapsed_ms: 5,
        running: true,
        counts: TestCounts::default(),
        output_chunks: vec![TestOutputChunk {
            sequence: 0,
            stream: TestOutputStream::Stdout,
            text: "running".into(),
        }],
        output: "running".into(),
        omitted_output_bytes: 0,
    };
    let retained = RetainedTestResult {
        run_id: active.run_id.clone(),
        identity,
        command: active.command.clone(),
        working_dir: active.working_dir.clone(),
        started_at_ms: active.started_at_ms,
        duration_ms: 25,
        outcome: TestOutcome::Passed,
        counts: TestCounts {
            passed: 1,
            ..TestCounts::default()
        },
        exit_code: Some(0),
        failed_tests: Vec::new(),
        output: "test result: ok".into(),
        omitted_output_bytes: 0,
    };

    assert_eq!(active.output, "running");
    assert_eq!(active.elapsed_ms, 5);
    assert!(active.running);
    assert_eq!(retained.outcome, TestOutcome::Passed);
    assert_eq!(retained.counts.passed, 1);
    assert_eq!(retained.duration_ms, 25);
}

// covers: deepseek-custom/test-suite-control :: Test runs are serialized and observable :: A test run starts
#[test]
fn active_run_projects_identity_command_time_elapsed_and_ordered_output() {
    let root = PathBuf::from("fixed-project-root");
    let discovery = discovered_state(80);
    let identity = discovery.catalogue.as_ref().unwrap().full_workspace.clone();
    let request = TestRunRequest {
        identity: identity.clone(),
        catalogue_revision: 80,
    };
    let executor = ScriptedExecutor {
        chunks: vec![
            TestOutputChunk {
                sequence: 1,
                stream: TestOutputStream::Stderr,
                text: "second\n".into(),
            },
            TestOutputChunk {
                sequence: 0,
                stream: TestOutputStream::Stdout,
                text: "first\n".into(),
            },
        ],
        exit: TestProcessExit { exit_code: Some(0) },
    };
    let mut runs = TestRunCoordinator::default();

    let started = runs
        .start(&discovery, &request, &root, &executor, &FixedClock(1_000))
        .unwrap();
    assert_eq!(started.identity, identity);
    assert_eq!(started.started_at_ms, 1_000);
    assert_eq!(started.elapsed_ms, 0);
    assert!(started.running);
    assert_eq!(started.command[0], "cargo");
    assert_eq!(started.working_dir, root);

    let result = runs.poll(&FixedClock(1_025)).unwrap().unwrap();
    assert_eq!(result.output, "first\nsecond\n");
    assert_eq!(result.duration_ms, 25);
}

// covers: deepseek-custom/test-suite-control :: Test runs are serialized and observable :: Another run is requested concurrently
#[test]
fn concurrent_run_reports_active_identity_without_starting_another_executor() {
    let root = PathBuf::from("fixed-project-root");
    let discovery = discovered_state(81);
    let request = TestRunRequest {
        identity: discovery.catalogue.as_ref().unwrap().full_workspace.clone(),
        catalogue_revision: 81,
    };
    let executor = RecordingExecutor::default();
    let mut runs = TestRunCoordinator::default();
    let active_id = runs
        .start(&discovery, &request, &root, &executor, &FixedClock(2_000))
        .unwrap()
        .run_id
        .clone();

    assert!(matches!(
        runs.start(&discovery, &request, &root, &executor, &FixedClock(2_001)),
        Err(TestRunRequestError::ActiveRun { active_run_id }) if active_run_id == active_id
    ));
    assert_eq!(executor.invocations.lock().unwrap().len(), 1);
}

// covers: deepseek-custom/test-suite-control :: Test runs are serialized and observable :: A test run finishes
#[test]
fn terminal_run_retains_cargo_counts_failures_exit_duration_and_outcome() {
    let root = PathBuf::from("fixed-project-root");
    let discovery = discovered_state(82);
    let request = TestRunRequest {
        identity: discovery.catalogue.as_ref().unwrap().full_workspace.clone(),
        catalogue_revision: 82,
    };
    let output = concat!(
        "running 3 tests\n",
        "test application_actor::passes ... ok\n",
        "test application_actor::fails ... FAILED\n\n",
        "failures:\n\n",
        "---- application_actor::fails stdout ----\n",
        "assertion failed\n\n",
        "failures:\n",
        "    application_actor::fails\n\n",
        "test result: FAILED. 1 passed; 1 failed; 1 ignored; 0 measured; 2 filtered out\n",
    );
    let executor = ScriptedExecutor {
        chunks: vec![TestOutputChunk {
            sequence: 0,
            stream: TestOutputStream::Stdout,
            text: output.into(),
        }],
        exit: TestProcessExit {
            exit_code: Some(101),
        },
    };
    let mut runs = TestRunCoordinator::default();
    runs.start(&discovery, &request, &root, &executor, &FixedClock(3_000))
        .unwrap();

    let result = runs.poll(&FixedClock(3_075)).unwrap().unwrap();
    assert_eq!(result.outcome, TestOutcome::Failed);
    assert_eq!(result.counts.passed, 1);
    assert_eq!(result.counts.failed, 1);
    assert_eq!(result.counts.ignored, 1);
    assert_eq!(result.counts.filtered, 2);
    assert_eq!(result.failed_tests, ["application_actor::fails"]);
    assert_eq!(result.exit_code, Some(101));
    assert_eq!(result.duration_ms, 75);
    assert!(result.output.contains("assertion failed"));
    assert_eq!(
        classify_test_outcome(TestProcessExit { exit_code: None }, false, true),
        TestOutcome::Cancelled
    );
    assert_eq!(
        classify_test_outcome(
            TestProcessExit {
                exit_code: Some(101)
            },
            false,
            false
        ),
        TestOutcome::InfrastructureError
    );
}

// covers: deepseek-custom/test-suite-control :: Test cancellation reaps the complete process tree :: User cancels an active test run
#[test]
fn cancellation_records_terminal_outcome_releases_slot_and_allows_later_run() {
    let root = PathBuf::from("fixed-project-root");
    let discovery = discovered_state(83);
    let request = TestRunRequest {
        identity: discovery.catalogue.as_ref().unwrap().full_workspace.clone(),
        catalogue_revision: 83,
    };
    let executor = ScriptedExecutor {
        chunks: vec![TestOutputChunk {
            sequence: 0,
            stream: TestOutputStream::Stdout,
            text: "running 1 test\n".into(),
        }],
        exit: TestProcessExit { exit_code: Some(0) },
    };
    let mut runs = TestRunCoordinator::default();
    let first_id = runs
        .start(&discovery, &request, &root, &executor, &FixedClock(4_000))
        .unwrap()
        .run_id
        .clone();

    let cancelled = runs.cancel(&FixedClock(4_025)).unwrap().unwrap();

    assert_eq!(cancelled.run_id, first_id);
    assert_eq!(cancelled.outcome, TestOutcome::Cancelled);
    assert_eq!(cancelled.duration_ms, 25);
    assert!(runs.active.is_none());
    let later = runs
        .start(&discovery, &request, &root, &executor, &FixedClock(4_100))
        .unwrap();
    assert_ne!(later.run_id, first_id);
}

// covers: deepseek-custom/test-suite-control :: Test cancellation reaps the complete process tree :: Browser disconnects during a test run
#[test]
fn reconnect_snapshot_restores_active_test_without_owning_its_execution() {
    let root = PathBuf::from("fixed-project-root");
    let discovery = discovered_state(84);
    let request = TestRunRequest {
        identity: discovery.catalogue.as_ref().unwrap().full_workspace.clone(),
        catalogue_revision: 84,
    };
    let executor = RunningExecutor;
    let mut runs = TestRunCoordinator::default();
    let active_id = runs
        .start(&discovery, &request, &root, &executor, &FixedClock(5_000))
        .unwrap()
        .run_id
        .clone();

    // A browser receives only this serialized projection. Dropping it cannot
    // drop the coordinator's process handle or affect the active slot.
    let disconnected_projection = runs.snapshot(&discovery);
    let wire = serde_json::to_string(&disconnected_projection).unwrap();
    drop(disconnected_projection);
    assert_eq!(runs.active.as_ref().unwrap().run_id, active_id);
    runs.poll(&FixedClock(5_020)).unwrap();

    let reconnected: TestControlSnapshot =
        serde_json::from_str(&serde_json::to_string(&runs.snapshot(&discovery)).unwrap()).unwrap();
    assert_eq!(reconnected.active.as_ref().unwrap().run_id, active_id);
    assert_eq!(
        reconnected.active.as_ref().unwrap().output,
        "still running\n"
    );
    assert!(wire.contains(&active_id));
}

#[cfg(windows)]
#[test]
fn cargo_executor_cancellation_reaps_a_real_windows_descendant() {
    use std::time::{Duration, Instant};

    let dir = super::scratch_dir("cargo-test-tree", "cancel");
    let pid_file = dir.join("descendant.pid");
    let script = format!(
        "$child = Start-Process ping -ArgumentList '-t','127.0.0.1' -PassThru; Set-Content -LiteralPath '{}' -Value $child.Id; while ($true) {{ Start-Sleep -Milliseconds 100 }}",
        pid_file.display()
    );
    let invocation = TestInvocation {
        program: "powershell".into(),
        args: vec!["-NoProfile".into(), "-Command".into(), script],
        working_dir: dir.clone(),
    };
    let mut execution = CargoTestExecutor.start(&invocation).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !pid_file.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    let descendant_pid: u32 = std::fs::read_to_string(&pid_file)
        .expect("parent must report its descendant pid")
        .trim()
        .parse()
        .unwrap();

    execution.cancel_and_wait().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut alive = true;
    while alive && Instant::now() < deadline {
        let output = std::process::Command::new("tasklist")
            .args([
                "/FI",
                &format!("PID eq {descendant_pid}"),
                "/FO",
                "CSV",
                "/NH",
            ])
            .output()
            .unwrap();
        alive = String::from_utf8_lossy(&output.stdout).contains(&descendant_pid.to_string());
        if alive {
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    eprintln!("cancelled Cargo tree descendant pid={descendant_pid}, alive_after_reap={alive}");
    assert!(
        !alive,
        "descendant process {descendant_pid} survived cancellation"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

// covers: deepseek-custom/test-suite-control :: The test catalogue reflects the repository test target :: Test discovery succeeds
#[test]
fn discovery_groups_exact_names_and_offers_only_the_approved_workspace_shape() {
    let root = PathBuf::from("fixed-project-root");
    let executor = ScriptedExecutor {
        chunks: vec![TestOutputChunk {
            sequence: 0,
            stream: TestOutputStream::Stdout,
            text: concat!(
                "application_actor::second: test\n",
                "application_actor::first: test\n",
                "web_server::serves_health: test\n",
                "2 tests, 0 benchmarks\n"
            )
            .into(),
        }],
        exit: TestProcessExit { exit_code: Some(0) },
    };
    let mut state = TestDiscoveryState::default();

    let catalogue = state.refresh(&root, &executor, &FixedClock(42)).unwrap();

    assert_eq!(catalogue.discovered_at_ms, 42);
    assert_eq!(catalogue.modules.len(), 2);
    assert_eq!(catalogue.modules[0].name, "application_actor");
    assert_eq!(
        catalogue.modules[0].tests[0].name,
        "application_actor::first"
    );
    assert_eq!(
        catalogue.modules[0].tests[1].name,
        "application_actor::second"
    );
    assert_eq!(catalogue.modules[1].name, "web_server");
    assert_eq!(catalogue.full_workspace.scope, TestScope::FullWorkspace);
    assert_eq!(
        full_workspace_test_invocation(&root),
        TestInvocation {
            program: "cargo".into(),
            args: vec![
                "test".into(),
                "--workspace".into(),
                "-j".into(),
                "1".into(),
                "--".into(),
                "--test-threads=1".into(),
            ],
            working_dir: root.clone(),
        }
    );
    assert_eq!(
        integration_test_discovery_invocation(&root).working_dir,
        root
    );
    assert!(!state.catalogue_stale);
    assert!(state.failure.is_none());
}

#[derive(Default)]
struct RecordingExecutor {
    invocations: Arc<Mutex<Vec<TestInvocation>>>,
}

impl TestExecutor for RecordingExecutor {
    fn start(
        &self,
        invocation: &TestInvocation,
    ) -> Result<Box<dyn TestExecution>, TestExecutionError> {
        self.invocations.lock().unwrap().push(invocation.clone());
        Ok(Box::new(ScriptedExecution {
            chunks: Vec::new().into_iter(),
            exit: TestProcessExit { exit_code: Some(0) },
        }))
    }
}

fn discovered_state(revision: u64) -> TestDiscoveryState {
    let executor = ScriptedExecutor {
        chunks: vec![TestOutputChunk {
            sequence: 0,
            stream: TestOutputStream::Stdout,
            text: concat!(
                "application_actor::dispatches: test\n",
                "web_server::serves_health: test\n"
            )
            .into(),
        }],
        exit: TestProcessExit { exit_code: Some(0) },
    };
    let mut state = TestDiscoveryState::default();
    state
        .refresh(
            PathBuf::from("discovery-root").as_path(),
            &executor,
            &FixedClock(revision),
        )
        .unwrap();
    state
}

// covers: deepseek-custom/test-suite-control :: Test execution is constrained to approved suite shapes :: User runs the full suite
#[test]
fn full_suite_uses_fixed_root_and_records_serialized_workspace_argv() {
    let root = PathBuf::from("fixed-project-root");
    let state = discovered_state(73);
    let executor = RecordingExecutor::default();
    let request = TestRunRequest {
        identity: state.catalogue.as_ref().unwrap().full_workspace.clone(),
        catalogue_revision: 73,
    };

    let (invocation, _) = state.start_run(&request, &root, &executor).unwrap();

    assert_eq!(invocation.program, "cargo");
    assert_eq!(
        invocation.args,
        ["test", "--workspace", "-j", "1", "--", "--test-threads=1"]
    );
    assert_eq!(invocation.working_dir, root);
    assert_eq!(
        executor.invocations.lock().unwrap().as_slice(),
        &[invocation]
    );
}

// covers: deepseek-custom/test-suite-control :: Test execution is constrained to approved suite shapes :: User runs one module or test
#[test]
fn module_and_exact_runs_use_server_owned_focused_argv() {
    let root = PathBuf::from("fixed-project-root");
    let state = discovered_state(74);
    let executor = RecordingExecutor::default();
    let catalogue = state.catalogue.as_ref().unwrap();
    let module = TestIdentity {
        name: catalogue.modules[0].name.clone(),
        scope: TestScope::Module {
            module: catalogue.modules[0].name.clone(),
        },
    };
    let exact = catalogue.modules[1].tests[0].clone();

    let (module_invocation, _) = state
        .start_run(
            &TestRunRequest {
                identity: module,
                catalogue_revision: 74,
            },
            &root,
            &executor,
        )
        .unwrap();
    let (exact_invocation, _) = state
        .start_run(
            &TestRunRequest {
                identity: exact,
                catalogue_revision: 74,
            },
            &root,
            &executor,
        )
        .unwrap();

    assert_eq!(
        module_invocation.args,
        [
            "test",
            "-p",
            "deepseek-custom-tests",
            "--test",
            "it",
            "application_actor::",
            "--",
            "--test-threads=1",
        ]
    );
    assert_eq!(
        exact_invocation.args,
        [
            "test",
            "-p",
            "deepseek-custom-tests",
            "--test",
            "it",
            "web_server::serves_health",
            "--",
            "--exact",
            "--test-threads=1",
        ]
    );
    assert_eq!(module_invocation.working_dir, root);
    assert_eq!(exact_invocation.working_dir, root);
    assert_eq!(executor.invocations.lock().unwrap().len(), 2);
}

// covers: deepseek-custom/test-suite-control :: Test execution is constrained to approved suite shapes :: Client submits an unknown test identity
#[test]
fn rejected_client_run_data_never_reaches_process_spawn() {
    let root = PathBuf::from("fixed-project-root");
    let state = discovered_state(75);
    let executor = RecordingExecutor::default();
    let unknown = TestRunRequest {
        identity: TestIdentity {
            name: "application_actor::not_discovered".into(),
            scope: TestScope::Exact {
                module: "application_actor".into(),
                test: "application_actor::not_discovered".into(),
            },
        },
        catalogue_revision: 75,
    };

    assert!(matches!(
        state.start_run(&unknown, &root, &executor),
        Err(TestRunRequestError::UnknownIdentity(_))
    ));
    assert!(matches!(
        state.start_run(
            &TestRunRequest {
                identity: state.catalogue.as_ref().unwrap().full_workspace.clone(),
                catalogue_revision: 74,
            },
            &root,
            &executor,
        ),
        Err(TestRunRequestError::StaleCatalogueRevision {
            expected: 75,
            received: 74
        })
    ));

    let mut stale = state.clone();
    stale.catalogue_stale = true;
    assert!(matches!(
        stale.start_run(
            &TestRunRequest {
                identity: stale.catalogue.as_ref().unwrap().full_workspace.clone(),
                catalogue_revision: 75,
            },
            &root,
            &executor,
        ),
        Err(TestRunRequestError::CatalogueStale)
    ));
    assert!(serde_json::from_str::<TestRunRequest>(
        r#"{"identity":{"name":"Full workspace","scope":{"type":"full_workspace"}},"catalogue_revision":75,"working_dir":"C:/client","command":"cmd","args":["/c"],"env":{"SECRET":"x"}}"#
    )
    .is_err());
    assert!(executor.invocations.lock().unwrap().is_empty());
}

// covers: deepseek-custom/test-suite-control :: The test catalogue reflects the repository test target :: Test discovery fails
#[test]
fn failed_discovery_retains_catalogue_and_bounded_exact_diagnostics() {
    let root = PathBuf::from("fixed-project-root");
    let successful = ScriptedExecutor {
        chunks: vec![TestOutputChunk {
            sequence: 0,
            stream: TestOutputStream::Stdout,
            text: "application_test_control::known: test\n".into(),
        }],
        exit: TestProcessExit { exit_code: Some(0) },
    };
    let mut state = TestDiscoveryState::default();
    state.refresh(&root, &successful, &FixedClock(10)).unwrap();
    let original = state.catalogue.clone().unwrap();
    let diagnostic = format!(
        "compile start\n{}\ncompile end",
        "x".repeat(TEST_DISCOVERY_DIAGNOSTIC_LIMIT_BYTES * 2)
    );
    let failing = ScriptedExecutor {
        chunks: vec![TestOutputChunk {
            sequence: 0,
            stream: TestOutputStream::Stderr,
            text: diagnostic,
        }],
        exit: TestProcessExit {
            exit_code: Some(101),
        },
    };

    let failure = state.refresh(&root, &failing, &FixedClock(20)).unwrap_err();

    assert_eq!(
        failure.command,
        vec![
            "cargo",
            "test",
            "-p",
            "deepseek-custom-tests",
            "--test",
            "it",
            "--",
            "--list",
            "--format",
            "terse",
        ]
    );
    assert_eq!(failure.exit_code, Some(101));
    assert_eq!(failure.working_dir, root);
    assert!(failure.diagnostic_output.starts_with("compile start"));
    assert!(failure.diagnostic_output.ends_with("compile end"));
    assert!(failure.diagnostic_output.contains("bytes omitted"));
    assert!(failure.omitted_output_bytes > 0);
    assert_eq!(state.catalogue, Some(original));
    assert!(state.catalogue_stale);
}

// covers: deepseek-custom/test-suite-control :: Test results are retained with explicit limits :: User revisits recent results
#[test]
fn terminal_results_are_atomically_loaded_newest_first_with_complete_details() {
    let root = super::scratch_dir("test-results", "revisit");
    let store = TestResultStore::new(&root);
    let older = retained_result(7);
    let newer = retained_result(9);
    store.store(&older).unwrap();
    store.store(&newer).unwrap();

    let loaded = store.load_recent().unwrap();
    assert!(loaded.warnings.is_empty());
    assert_eq!(loaded.results, vec![newer.clone(), older]);
    assert_eq!(loaded.results[0].identity, newer.identity);
    assert_eq!(loaded.results[0].outcome, TestOutcome::Failed);
    assert_eq!(loaded.results[0].counts.failed, 1);
    assert_eq!(loaded.results[0].duration_ms, 90);
    assert_eq!(loaded.results[0].command[0], "cargo");
    assert_eq!(loaded.results[0].output, "diagnostic output 9\n");
    let entries = std::fs::read_dir(store.directory())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), 2);
    assert!(
        entries
            .iter()
            .all(|path| path.extension().unwrap() == "json")
    );

    let snapshot = TestRunCoordinator::default()
        .snapshot_with_results(&TestDiscoveryState::default(), &store)
        .unwrap();
    let wire = serde_json::to_value(snapshot).unwrap();
    assert_eq!(wire["retained_results"].as_array().unwrap().len(), 2);
    assert_eq!(
        wire["retained_results"][0]["identity"]["name"],
        newer.identity.name
    );
    assert_eq!(wire["retained_results"][0]["command"][0], "cargo");
    assert_eq!(
        wire["retained_results"][0]["output"],
        "diagnostic output 9\n"
    );

    std::fs::write(
        store.directory().join("99999999999999999999-broken.json"),
        b"{",
    )
    .unwrap();
    let with_malformed = store.load_recent().unwrap();
    assert_eq!(with_malformed.results.len(), 2);
    assert_eq!(with_malformed.warnings.len(), 1);
    assert!(with_malformed.warnings[0].contains("broken.json"));
    std::fs::remove_dir_all(root).unwrap();
}

// covers: deepseek-custom/test-suite-control :: Test results are retained with explicit limits :: Retention limit is exceeded
#[test]
fn twenty_first_terminal_result_prunes_only_the_oldest_record() {
    let root = super::scratch_dir("test-results", "retention");
    let store = TestResultStore::new(&root);
    for number in 1..=21 {
        store.store(&retained_result(number)).unwrap();
    }
    let loaded = store.load_recent().unwrap();
    assert!(loaded.warnings.is_empty());
    assert_eq!(loaded.results.len(), 20);
    assert_eq!(loaded.results.first().unwrap().run_id, "run-21");
    assert_eq!(loaded.results.last().unwrap().run_id, "run-2");
    assert!(!loaded.results.iter().any(|result| result.run_id == "run-1"));
    assert_eq!(std::fs::read_dir(store.directory()).unwrap().count(), 20);

    let active_record = root.join("active-run.live");
    std::fs::write(&active_record, b"owned by the active process slot").unwrap();
    store.store(&retained_result(22)).unwrap();
    assert!(active_record.exists());
    assert_eq!(store.load_recent().unwrap().results[0].run_id, "run-22");
    std::fs::remove_dir_all(root).unwrap();
}
