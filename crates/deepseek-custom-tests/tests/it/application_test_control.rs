use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use deepseek_custom::application::test_control::{
    ActiveTestSlot, RetainedTestResult, TEST_DISCOVERY_DIAGNOSTIC_LIMIT_BYTES, TestClock,
    TestCounts, TestDiscoveryState, TestExecution, TestExecutionError, TestExecutor, TestIdentity,
    TestInvocation, TestOutcome, TestOutputChunk, TestOutputStream, TestProcessExit,
    TestRunRequest, TestRunRequestError, TestScope, full_workspace_test_invocation,
    integration_test_discovery_invocation,
};

struct FixedClock(u64);

impl TestClock for FixedClock {
    fn now_ms(&self) -> u64 {
        self.0
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
        counts: TestCounts::default(),
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
    assert_eq!(retained.outcome, TestOutcome::Passed);
    assert_eq!(retained.counts.passed, 1);
    assert_eq!(retained.duration_ms, 25);
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
