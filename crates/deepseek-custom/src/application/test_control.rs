//! Presentation-neutral contracts for repository test discovery and execution.
//!
//! This module defines the test-control domain without coupling it to Cargo,
//! browser connections, persistence, or the application actor. Later adapters
//! can own those concerns behind the executor and clock seams below.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde::{Deserialize, Serialize};

use crate::mcp::spawn::resolve_command;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestCatalogue {
    pub discovered_at_ms: u64,
    pub full_workspace: TestIdentity,
    pub modules: Vec<TestModule>,
}

pub const TEST_DISCOVERY_DIAGNOSTIC_LIMIT_BYTES: usize = 32 * 1024;

const INTEGRATION_PACKAGE: &str = "deepseek-custom-tests";
const INTEGRATION_TARGET: &str = "it";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestDiscoveryFailure {
    pub command: Vec<String>,
    pub working_dir: PathBuf,
    pub exit_code: Option<i32>,
    pub diagnostic_output: String,
    pub omitted_output_bytes: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestDiscoveryState {
    pub catalogue: Option<TestCatalogue>,
    pub catalogue_stale: bool,
    pub failure: Option<TestDiscoveryFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestModule {
    pub name: String,
    pub tests: Vec<TestIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TestIdentity {
    pub name: String,
    pub scope: TestScope,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TestScope {
    FullWorkspace,
    Module { module: String },
    Exact { module: String, test: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestRunRequest {
    pub identity: TestIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestInvocation {
    pub program: String,
    pub args: Vec<String>,
    pub working_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestOutputStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestOutputChunk {
    pub sequence: u64,
    pub stream: TestOutputStream,
    pub text: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestCounts {
    pub passed: u64,
    pub failed: u64,
    pub ignored: u64,
    pub filtered: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestOutcome {
    Passed,
    Failed,
    Cancelled,
    InfrastructureError,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetainedTestResult {
    pub run_id: String,
    pub identity: TestIdentity,
    pub command: Vec<String>,
    pub working_dir: PathBuf,
    pub started_at_ms: u64,
    pub duration_ms: u64,
    pub outcome: TestOutcome,
    pub counts: TestCounts,
    pub exit_code: Option<i32>,
    pub failed_tests: Vec<String>,
    pub output: String,
    pub omitted_output_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveTestSlot {
    pub run_id: String,
    pub identity: TestIdentity,
    pub command: Vec<String>,
    pub working_dir: PathBuf,
    pub started_at_ms: u64,
    pub counts: TestCounts,
    pub output: String,
    pub omitted_output_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TestProcessExit {
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct TestExecutionError {
    pub message: String,
}

/// Active process boundary used by discovery and run coordinators.
///
/// Keeping output, completion, and cancellation separate lets a deterministic
/// test double advance one event at a time without sleeping or starting Cargo.
pub trait TestExecution: Send {
    fn next_output(&mut self) -> Result<Option<TestOutputChunk>, TestExecutionError>;
    fn try_wait(&mut self) -> Result<Option<TestProcessExit>, TestExecutionError>;
    fn cancel_and_wait(&mut self) -> Result<TestProcessExit, TestExecutionError>;
}

/// Injectable process factory. Production adapters own the Cargo process tree.
pub trait TestExecutor: Send + Sync {
    fn start(
        &self,
        invocation: &TestInvocation,
    ) -> Result<Box<dyn TestExecution>, TestExecutionError>;
}

/// Time boundary used for stable timestamps and elapsed-time tests.
pub trait TestClock: Send + Sync {
    fn now_ms(&self) -> u64;
}

/// Build the one server-owned command used to enumerate the integration target.
pub fn integration_test_discovery_invocation(project_root: &Path) -> TestInvocation {
    TestInvocation {
        program: "cargo".into(),
        args: vec![
            "test".into(),
            "-p".into(),
            INTEGRATION_PACKAGE.into(),
            "--test".into(),
            INTEGRATION_TARGET.into(),
            "--".into(),
            "--list".into(),
            "--format".into(),
            "terse".into(),
        ],
        working_dir: project_root.to_path_buf(),
    }
}

/// The only whole-repository run offered by the catalogue.
pub fn full_workspace_test_invocation(project_root: &Path) -> TestInvocation {
    TestInvocation {
        program: "cargo".into(),
        args: vec![
            "test".into(),
            "--workspace".into(),
            "--".into(),
            "--test-threads=1".into(),
        ],
        working_dir: project_root.to_path_buf(),
    }
}

impl TestDiscoveryState {
    /// Refresh from Cargo while retaining the last successful catalogue on failure.
    pub fn refresh(
        &mut self,
        project_root: &Path,
        executor: &dyn TestExecutor,
        clock: &dyn TestClock,
    ) -> Result<&TestCatalogue, &TestDiscoveryFailure> {
        let invocation = integration_test_discovery_invocation(project_root);
        match run_discovery(&invocation, executor, clock.now_ms()) {
            Ok(catalogue) => {
                self.catalogue = Some(catalogue);
                self.catalogue_stale = false;
                self.failure = None;
                Ok(self.catalogue.as_ref().expect("catalogue was just stored"))
            }
            Err(failure) => {
                self.catalogue_stale = self.catalogue.is_some();
                self.failure = Some(failure);
                Err(self.failure.as_ref().expect("failure was just stored"))
            }
        }
    }
}

fn run_discovery(
    invocation: &TestInvocation,
    executor: &dyn TestExecutor,
    discovered_at_ms: u64,
) -> Result<TestCatalogue, TestDiscoveryFailure> {
    let command = invocation_command(invocation);
    let mut execution = executor.start(invocation).map_err(|error| {
        discovery_failure(invocation, command.clone(), None, error.message.as_bytes())
    })?;
    let mut output = Vec::new();
    loop {
        match execution.next_output() {
            Ok(Some(chunk)) => output.extend_from_slice(chunk.text.as_bytes()),
            Ok(None) => break,
            Err(error) => {
                output.extend_from_slice(error.message.as_bytes());
                return Err(discovery_failure(invocation, command, None, &output));
            }
        }
    }
    let exit = execution.try_wait().map_err(|error| {
        discovery_failure(invocation, command.clone(), None, error.message.as_bytes())
    })?;
    let Some(exit) = exit else {
        return Err(discovery_failure(
            invocation,
            command,
            None,
            b"test discovery stopped producing output before it exited",
        ));
    };
    if exit.exit_code != Some(0) {
        return Err(discovery_failure(
            invocation,
            command,
            exit.exit_code,
            &output,
        ));
    }
    parse_test_catalogue(&String::from_utf8_lossy(&output), discovered_at_ms).map_err(|message| {
        output.extend_from_slice(message.as_bytes());
        discovery_failure(invocation, command, exit.exit_code, &output)
    })
}

pub fn parse_test_catalogue(output: &str, discovered_at_ms: u64) -> Result<TestCatalogue, String> {
    let mut grouped = BTreeMap::<String, Vec<TestIdentity>>::new();
    for line in output.lines() {
        let Some(test_name) = line.strip_suffix(": test") else {
            continue;
        };
        let Some((module, _)) = test_name.split_once("::") else {
            continue;
        };
        grouped
            .entry(module.into())
            .or_default()
            .push(TestIdentity {
                name: test_name.into(),
                scope: TestScope::Exact {
                    module: module.into(),
                    test: test_name.into(),
                },
            });
    }
    if grouped.is_empty() {
        return Err("Cargo listed no integration tests".into());
    }
    let modules = grouped
        .into_iter()
        .map(|(name, mut tests)| {
            tests.sort_by(|left, right| left.name.cmp(&right.name));
            TestModule { name, tests }
        })
        .collect();
    Ok(TestCatalogue {
        discovered_at_ms,
        full_workspace: TestIdentity {
            name: "Full workspace".into(),
            scope: TestScope::FullWorkspace,
        },
        modules,
    })
}

fn invocation_command(invocation: &TestInvocation) -> Vec<String> {
    std::iter::once(invocation.program.clone())
        .chain(invocation.args.iter().cloned())
        .collect()
}

fn discovery_failure(
    invocation: &TestInvocation,
    command: Vec<String>,
    exit_code: Option<i32>,
    output: &[u8],
) -> TestDiscoveryFailure {
    let (diagnostic_output, omitted_output_bytes) = bounded_diagnostic(output);
    TestDiscoveryFailure {
        command,
        working_dir: invocation.working_dir.clone(),
        exit_code,
        diagnostic_output,
        omitted_output_bytes,
    }
}

fn bounded_diagnostic(output: &[u8]) -> (String, u64) {
    if output.len() <= TEST_DISCOVERY_DIAGNOSTIC_LIMIT_BYTES {
        return (String::from_utf8_lossy(output).into_owned(), 0);
    }
    const MARKER_RESERVE_BYTES: usize = 64;
    let side = (TEST_DISCOVERY_DIAGNOSTIC_LIMIT_BYTES - MARKER_RESERVE_BYTES) / 2;
    let omitted = output.len() - side * 2;
    let text = format!(
        "{}\n...[{} bytes omitted]...\n{}",
        String::from_utf8_lossy(&output[..side]),
        omitted,
        String::from_utf8_lossy(&output[output.len() - side..])
    );
    (text, omitted as u64)
}

/// Production executor for Cargo discovery. Run execution gets a separate,
/// cancellable process-tree adapter in the later execution tasks.
#[derive(Debug, Default)]
pub struct CargoTestDiscoveryExecutor;

impl TestExecutor for CargoTestDiscoveryExecutor {
    fn start(
        &self,
        invocation: &TestInvocation,
    ) -> Result<Box<dyn TestExecution>, TestExecutionError> {
        let resolved = resolve_command(&invocation.program);
        let output = Command::new(&resolved.program)
            .args(&resolved.prefix_args)
            .args(&invocation.args)
            .current_dir(&invocation.working_dir)
            .stdin(Stdio::null())
            .output()
            .map_err(|error| TestExecutionError {
                message: format!("failed to start test discovery: {error}"),
            })?;
        Ok(Box::new(CompletedDiscoveryExecution::new(output)))
    }
}

struct CompletedDiscoveryExecution {
    chunks: std::vec::IntoIter<TestOutputChunk>,
    exit: TestProcessExit,
}

impl CompletedDiscoveryExecution {
    fn new(output: std::process::Output) -> Self {
        let mut chunks = Vec::new();
        if !output.stdout.is_empty() {
            chunks.push(TestOutputChunk {
                sequence: 0,
                stream: TestOutputStream::Stdout,
                text: String::from_utf8_lossy(&output.stdout).into_owned(),
            });
        }
        if !output.stderr.is_empty() {
            chunks.push(TestOutputChunk {
                sequence: chunks.len() as u64,
                stream: TestOutputStream::Stderr,
                text: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        Self {
            chunks: chunks.into_iter(),
            exit: TestProcessExit {
                exit_code: output.status.code(),
            },
        }
    }
}

impl TestExecution for CompletedDiscoveryExecution {
    fn next_output(&mut self) -> Result<Option<TestOutputChunk>, TestExecutionError> {
        Ok(self.chunks.next())
    }

    fn try_wait(&mut self) -> Result<Option<TestProcessExit>, TestExecutionError> {
        Ok(Some(self.exit))
    }

    fn cancel_and_wait(&mut self) -> Result<TestProcessExit, TestExecutionError> {
        Ok(self.exit)
    }
}
