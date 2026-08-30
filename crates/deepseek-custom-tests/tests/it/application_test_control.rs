use std::path::PathBuf;

use deepseek_custom::application::test_control::{
    ActiveTestSlot, RetainedTestResult, TestClock, TestCounts, TestExecution, TestExecutionError,
    TestExecutor, TestIdentity, TestInvocation, TestOutcome, TestOutputChunk, TestOutputStream,
    TestProcessExit, TestRunRequest, TestScope,
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
