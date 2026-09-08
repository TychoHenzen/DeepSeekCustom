//! Presentation-neutral contracts for repository test discovery and execution.
//!
//! This module defines the test-control domain without coupling it to Cargo,
//! browser connections, persistence, or the application actor. Later adapters
//! can own those concerns behind the executor and clock seams below.

use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::mcp::spawn::resolve_command;

pub use super::test_run_coordinator::TestRunCoordinator;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestCatalogue {
    pub discovered_at_ms: u64,
    pub full_workspace: TestIdentity,
    pub modules: Vec<TestModule>,
}

pub const TEST_DISCOVERY_DIAGNOSTIC_LIMIT_BYTES: usize = 32 * 1024;
pub const RETAINED_TEST_RESULT_LIMIT: usize = 20;
pub const TEST_OUTPUT_LIMIT_BYTES: usize = 4 * 1024 * 1024;

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
#[serde(deny_unknown_fields)]
pub struct TestRunRequest {
    pub identity: TestIdentity,
    pub catalogue_revision: u64,
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
    pub elapsed_ms: u64,
    pub running: bool,
    pub counts: TestCounts,
    pub output_chunks: Vec<TestOutputChunk>,
    pub output: String,
    pub omitted_output_bytes: u64,
}

/// Browser-safe projection of the test service's current process-owned state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestControlSnapshot {
    pub discovery: TestDiscoveryState,
    pub active: Option<ActiveTestSlot>,
    pub latest_result: Option<RetainedTestResult>,
    /// Newest-first terminal results loaded from the fixed project store.
    #[serde(default)]
    pub retained_results: Vec<RetainedTestResult>,
    /// Files that could not be read or decoded. One bad record does not hide
    /// the remaining test history from the browser.
    #[serde(default)]
    pub retained_result_warnings: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadedTestResults {
    pub results: Vec<RetainedTestResult>,
    pub warnings: Vec<String>,
}

/// Atomic, bounded persistence for terminal test results.
///
/// Active runs never enter this directory, so pruning cannot cancel or remove
/// an active process. File names begin with the stable start time to make the
/// retention order independent of directory enumeration and file timestamps.
#[derive(Debug, Clone)]
pub struct TestResultStore {
    directory: PathBuf,
}

impl TestResultStore {
    pub fn new(project_root: &Path) -> Self {
        Self {
            directory: project_root.join(".deepseek").join("test-runs"),
        }
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn store(&self, result: &RetainedTestResult) -> io::Result<()> {
        fs::create_dir_all(&self.directory)?;
        let stem = retained_result_stem(result);
        let final_path = self.directory.join(format!("{stem}.json"));
        let temporary_path = self.directory.join(format!(".{stem}.tmp"));
        let bytes = serde_json::to_vec_pretty(result).map_err(io::Error::other)?;

        let write_result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary_path)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&temporary_path, &final_path)?;
            self.prune()?;
            Ok(())
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&temporary_path);
        }
        write_result
    }

    pub fn load_recent(&self) -> io::Result<LoadedTestResults> {
        if !self.directory.exists() {
            return Ok(LoadedTestResults::default());
        }
        let mut loaded = LoadedTestResults::default();
        for path in self.result_paths_newest_first()? {
            match fs::read(&path).and_then(|bytes| {
                serde_json::from_slice::<RetainedTestResult>(&bytes).map_err(io::Error::other)
            }) {
                Ok(result) => loaded.results.push(result),
                Err(error) => loaded.warnings.push(format!(
                    "could not load retained test result {}: {error}",
                    path.display()
                )),
            }
        }
        loaded.results.truncate(RETAINED_TEST_RESULT_LIMIT);
        Ok(loaded)
    }

    fn prune(&self) -> io::Result<()> {
        let paths = self.result_paths_newest_first()?;
        for path in paths.into_iter().skip(RETAINED_TEST_RESULT_LIMIT) {
            fs::remove_file(path)?;
        }
        Ok(())
    }

    fn result_paths_newest_first(&self) -> io::Result<Vec<PathBuf>> {
        let mut paths = fs::read_dir(&self.directory)?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .collect::<Vec<_>>();
        paths.sort_by(|left, right| right.file_name().cmp(&left.file_name()));
        Ok(paths)
    }
}

fn retained_result_stem(result: &RetainedTestResult) -> String {
    let safe_run_id = result
        .run_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("{:020}-{safe_run_id}", result.started_at_ms)
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

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TestRunRequestError {
    #[error("test run {active_run_id} is already active")]
    ActiveRun { active_run_id: String },
    #[error("test catalogue has not been discovered")]
    CatalogueUnavailable,
    #[error("test catalogue is stale and must be refreshed")]
    CatalogueStale,
    #[error("stale test catalogue revision: expected {expected}, received {received}")]
    StaleCatalogueRevision { expected: u64, received: u64 },
    #[error("unknown test identity: {0}")]
    UnknownIdentity(String),
    #[error(transparent)]
    Execution(#[from] TestExecutionError),
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

#[derive(Debug, Default)]
pub struct SystemTestClock;

impl TestClock for SystemTestClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }
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
            "-j".into(),
            "1".into(),
            "--".into(),
            "--test-threads=1".into(),
        ],
        working_dir: project_root.to_path_buf(),
    }
}

fn focused_test_invocation(identity: &TestIdentity, project_root: &Path) -> TestInvocation {
    let filter = match &identity.scope {
        TestScope::Module { module } => format!("{module}::"),
        TestScope::Exact { test, .. } => test.clone(),
        TestScope::FullWorkspace => unreachable!("full workspace uses its own invocation"),
    };
    let mut args = vec![
        "test".into(),
        "-p".into(),
        INTEGRATION_PACKAGE.into(),
        "--test".into(),
        INTEGRATION_TARGET.into(),
        filter,
        "--".into(),
    ];
    if matches!(identity.scope, TestScope::Exact { .. }) {
        args.push("--exact".into());
    }
    args.push("--test-threads=1".into());
    TestInvocation {
        program: "cargo".into(),
        args,
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

    /// Validate a browser-selected catalogue identity before any process starts.
    ///
    /// The client supplies no executable, path, arguments, or environment. The
    /// invocation is reconstructed here from the current server catalogue.
    pub fn start_run(
        &self,
        request: &TestRunRequest,
        project_root: &Path,
        executor: &dyn TestExecutor,
    ) -> Result<(TestInvocation, Box<dyn TestExecution>), TestRunRequestError> {
        let catalogue = self
            .catalogue
            .as_ref()
            .ok_or(TestRunRequestError::CatalogueUnavailable)?;
        if self.catalogue_stale {
            return Err(TestRunRequestError::CatalogueStale);
        }
        if request.catalogue_revision != catalogue.discovered_at_ms {
            return Err(TestRunRequestError::StaleCatalogueRevision {
                expected: catalogue.discovered_at_ms,
                received: request.catalogue_revision,
            });
        }

        let known_module = catalogue.modules.iter().any(|module| {
            request.identity.name == module.name
                && request.identity.scope
                    == (TestScope::Module {
                        module: module.name.clone(),
                    })
        });
        let known_exact = catalogue
            .modules
            .iter()
            .flat_map(|module| &module.tests)
            .any(|identity| identity == &request.identity);
        let invocation = if request.identity == catalogue.full_workspace {
            full_workspace_test_invocation(project_root)
        } else if known_module || known_exact {
            focused_test_invocation(&request.identity, project_root)
        } else {
            return Err(TestRunRequestError::UnknownIdentity(
                request.identity.name.clone(),
            ));
        };
        let execution = executor.start(&invocation)?;
        Ok((invocation, execution))
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

pub fn classify_test_outcome(
    exit: TestProcessExit,
    cargo_reported_result: bool,
    cancelled: bool,
) -> TestOutcome {
    if cancelled {
        TestOutcome::Cancelled
    } else if exit.exit_code == Some(0) {
        TestOutcome::Passed
    } else if cargo_reported_result {
        TestOutcome::Failed
    } else {
        TestOutcome::InfrastructureError
    }
}

/// Parse every Cargo/libtest result line because workspace runs can contain
/// several test binaries. Failed names come from libtest's final failure list.
pub fn parse_cargo_test_report(output: &str) -> (TestCounts, Vec<String>, bool) {
    let mut counts = TestCounts::default();
    let mut failed_tests = Vec::new();
    let mut cargo_reported_result = false;
    let mut reading_failed_names = false;

    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(summary) = trimmed.strip_prefix("test result: ") {
            cargo_reported_result = true;
            add_reported_count(summary, "passed", &mut counts.passed);
            add_reported_count(summary, "failed", &mut counts.failed);
            add_reported_count(summary, "ignored", &mut counts.ignored);
            add_reported_count(summary, "filtered out", &mut counts.filtered);
            reading_failed_names = false;
            continue;
        }
        if trimmed == "failures:" {
            reading_failed_names = true;
            continue;
        }
        if reading_failed_names {
            if trimmed.is_empty() || trimmed.starts_with("----") {
                continue;
            }
            if trimmed.starts_with("test result:") || trimmed.contains(" panicked at ") {
                reading_failed_names = false;
                continue;
            }
            if !trimmed.contains(char::is_whitespace)
                && trimmed.contains("::")
                && !failed_tests.iter().any(|name| name == trimmed)
            {
                failed_tests.push(trimmed.to_owned());
            }
        }
    }
    (counts, failed_tests, cargo_reported_result)
}

fn add_reported_count(summary: &str, label: &str, target: &mut u64) {
    for field in summary.split(';').map(str::trim) {
        let Some(value) = field.strip_suffix(label).map(str::trim) else {
            continue;
        };
        if let Some(number) = value.split_whitespace().last()
            && let Ok(number) = number.parse::<u64>()
        {
            *target = target.saturating_add(number);
        }
    }
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

/// Production executor for test runs whose Cargo descendants must be reaped.
#[derive(Debug, Default)]
pub struct CargoTestExecutor;

impl TestExecutor for CargoTestExecutor {
    fn start(
        &self,
        invocation: &TestInvocation,
    ) -> Result<Box<dyn TestExecution>, TestExecutionError> {
        CargoTestExecution::spawn(invocation).map(|execution| Box::new(execution) as Box<_>)
    }
}

enum CargoExecutionEvent {
    Output(TestOutputChunk),
    Exit(TestProcessExit),
    Error(String),
}

struct CargoTestExecution {
    events: mpsc::Receiver<CargoExecutionEvent>,
    cancel: mpsc::Sender<()>,
    terminal: Option<TestProcessExit>,
    pending_output: std::collections::VecDeque<TestOutputChunk>,
}

impl CargoTestExecution {
    fn spawn(invocation: &TestInvocation) -> Result<Self, TestExecutionError> {
        let invocation = invocation.clone();
        let (events_tx, events_rx) = mpsc::channel();
        let (cancel_tx, cancel_rx) = mpsc::channel();
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("cargo-test-process".into())
            .spawn(move || run_cargo_process(invocation, events_tx, cancel_rx, started_tx))
            .map_err(|error| execution_error("failed to create Cargo process owner", error))?;
        started_rx.recv().map_err(|error| TestExecutionError {
            message: format!("Cargo process owner stopped before spawn: {error}"),
        })??;
        Ok(Self {
            events: events_rx,
            cancel: cancel_tx,
            terminal: None,
            pending_output: std::collections::VecDeque::new(),
        })
    }

    fn receive(&mut self, blocking: bool) -> Result<Option<TestOutputChunk>, TestExecutionError> {
        let event = if blocking {
            self.events.recv().map_err(|error| TestExecutionError {
                message: format!("Cargo process owner disconnected: {error}"),
            })?
        } else {
            match self.events.try_recv() {
                Ok(event) => event,
                Err(mpsc::TryRecvError::Empty) => return Ok(None),
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(TestExecutionError {
                        message: "Cargo process owner disconnected before exit".into(),
                    });
                }
            }
        };
        match event {
            CargoExecutionEvent::Output(chunk) => Ok(Some(chunk)),
            CargoExecutionEvent::Exit(exit) => {
                self.terminal = Some(exit);
                Ok(None)
            }
            CargoExecutionEvent::Error(message) => Err(TestExecutionError { message }),
        }
    }
}

impl TestExecution for CargoTestExecution {
    fn next_output(&mut self) -> Result<Option<TestOutputChunk>, TestExecutionError> {
        if let Some(chunk) = self.pending_output.pop_front() {
            return Ok(Some(chunk));
        }
        self.receive(false)
    }

    fn try_wait(&mut self) -> Result<Option<TestProcessExit>, TestExecutionError> {
        while self.terminal.is_none() {
            match self.receive(false)? {
                Some(chunk) => self.pending_output.push_back(chunk),
                None => break,
            }
        }
        Ok(self.terminal)
    }

    fn cancel_and_wait(&mut self) -> Result<TestProcessExit, TestExecutionError> {
        if let Some(exit) = self.terminal {
            return Ok(exit);
        }
        let _ = self.cancel.send(());
        while self.terminal.is_none() {
            let _ = self.receive(true)?;
        }
        Ok(self
            .terminal
            .expect("blocking receive observed process exit"))
    }
}

fn run_cargo_process(
    invocation: TestInvocation,
    events: mpsc::Sender<CargoExecutionEvent>,
    cancel: mpsc::Receiver<()>,
    started: mpsc::SyncSender<Result<(), TestExecutionError>>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = started.send(Err(execution_error(
                "failed to create Cargo runtime",
                error,
            )));
            return;
        }
    };
    runtime.block_on(async move {
        use tokio::io::AsyncReadExt;

        let resolved = resolve_command(&invocation.program);
        let mut command = tokio::process::Command::new(&resolved.program);
        command
            .args(&resolved.prefix_args)
            .args(&invocation.args)
            .current_dir(&invocation.working_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        crate::process_group::prepare(&mut command);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                let _ = started.send(Err(execution_error("failed to start Cargo test", error)));
                return;
            }
        };
        #[cfg(windows)]
        let isolated_group = crate::process_group::adopt_isolated(&child);
        #[cfg(not(windows))]
        crate::process_group::adopt(&child);
        let mut stdout = child.stdout.take().expect("piped Cargo stdout");
        let mut stderr = child.stderr.take().expect("piped Cargo stderr");
        let _ = started.send(Ok(()));
        let mut sequence = 0_u64;
        let mut stdout_open = true;
        let mut stderr_open = true;
        let mut stdout_buffer = [0_u8; 8192];
        let mut stderr_buffer = [0_u8; 8192];
        loop {
            if cancel.try_recv().is_ok() {
                #[cfg(windows)]
                let terminated = crate::process_group::terminate_isolated(
                    isolated_group.as_ref(),
                    &mut child,
                );
                #[cfg(not(windows))]
                let terminated = crate::process_group::terminate(&mut child);
                if let Err(error) = terminated {
                    let _ = events.send(CargoExecutionEvent::Error(format!(
                        "failed to terminate Cargo process tree: {error}"
                    )));
                    return;
                }
                match child.wait().await {
                    Ok(status) => {
                        let _ = events.send(CargoExecutionEvent::Exit(TestProcessExit {
                            exit_code: status.code(),
                        }));
                    }
                    Err(error) => {
                        let _ = events.send(CargoExecutionEvent::Error(format!(
                            "failed to reap cancelled Cargo process tree: {error}"
                        )));
                    }
                }
                return;
            }

            tokio::select! {
                read = stdout.read(&mut stdout_buffer), if stdout_open => match read {
                    Ok(0) => stdout_open = false,
                    Ok(length) => send_output(&events, &mut sequence, TestOutputStream::Stdout, &stdout_buffer[..length]),
                    Err(error) => { let _ = events.send(CargoExecutionEvent::Error(format!("failed to read Cargo stdout: {error}"))); return; }
                },
                read = stderr.read(&mut stderr_buffer), if stderr_open => match read {
                    Ok(0) => stderr_open = false,
                    Ok(length) => send_output(&events, &mut sequence, TestOutputStream::Stderr, &stderr_buffer[..length]),
                    Err(error) => { let _ = events.send(CargoExecutionEvent::Error(format!("failed to read Cargo stderr: {error}"))); return; }
                },
                status = child.wait(), if !stdout_open && !stderr_open => {
                    match status {
                        Ok(status) => { let _ = events.send(CargoExecutionEvent::Exit(TestProcessExit { exit_code: status.code() })); }
                        Err(error) => { let _ = events.send(CargoExecutionEvent::Error(format!("failed to reap Cargo process tree: {error}"))); }
                    }
                    return;
                },
                _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {}
            }
        }
    });
}

fn send_output(
    events: &mpsc::Sender<CargoExecutionEvent>,
    sequence: &mut u64,
    stream: TestOutputStream,
    bytes: &[u8],
) {
    let chunk = TestOutputChunk {
        sequence: *sequence,
        stream,
        text: String::from_utf8_lossy(bytes).into_owned(),
    };
    *sequence = sequence.saturating_add(1);
    let _ = events.send(CargoExecutionEvent::Output(chunk));
}

fn execution_error(context: &str, error: impl Into<io::Error>) -> TestExecutionError {
    TestExecutionError {
        message: format!("{context}: {}", error.into()),
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
