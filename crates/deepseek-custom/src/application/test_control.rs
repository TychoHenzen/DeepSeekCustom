//! Presentation-neutral contracts for repository test discovery and execution.
//!
//! This module defines the test-control domain without coupling it to Cargo,
//! browser connections, persistence, or the application actor. Later adapters
//! can own those concerns behind the executor and clock seams below.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestCatalogue {
    pub discovered_at_ms: u64,
    pub full_workspace: TestIdentity,
    pub modules: Vec<TestModule>,
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
