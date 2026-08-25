//! Ordered deterministic commands for an isolated verification workspace.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use tokio::process::{Child, ChildStderr, ChildStdout, Command};
use tokio::sync::mpsc;

use super::{AppliedPatchWorkspace, GitApplyResult};
use crate::mcp::spawn::resolve_command;

/// Maximum bytes retained from either edge of a verifier stream.
pub const VERIFIER_OUTPUT_EDGE_BYTES: usize = 4 * 1024;

const OUTPUT_CHUNK_BYTES: usize = 4 * 1024;
const OUTPUT_BUFFER_BYTES: usize = VERIFIER_OUTPUT_EDGE_BYTES * 2;
const INTERRUPT_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The terminal state of one configured verifier command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifierCommandDisposition {
    Passed,
    Failed,
    SpawnFailed,
    Interrupted,
}

/// First and last output edges retained from one command stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundedVerifierOutput {
    pub text: String,
    pub first_edge: String,
    pub last_edge: String,
    pub truncated: bool,
    pub bytes_seen: u64,
}

impl BoundedVerifierOutput {
    fn empty() -> Self {
        Self {
            text: String::new(),
            first_edge: String::new(),
            last_edge: String::new(),
            truncated: false,
            bytes_seen: 0,
        }
    }
}

/// Evidence recorded for one configured verifier command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifierCommandResult {
    pub command: String,
    pub disposition: VerifierCommandDisposition,
    pub success: bool,
    pub exit_code: Option<i32>,
    pub stdout: BoundedVerifierOutput,
    pub stderr: BoundedVerifierOutput,
    pub combined_output: BoundedVerifierOutput,
    pub duration: Duration,
    pub error: Option<String>,
}

impl VerifierCommandResult {
    pub fn is_success(&self) -> bool {
        self.success
    }

    fn evidence(&self) -> VerifierCommandEvidence {
        VerifierCommandEvidence {
            command: self.command.clone(),
            disposition: self.disposition,
            success: self.success,
            exit_code: self.exit_code,
            stdout: self.stdout.clone(),
            stderr: self.stderr.clone(),
            combined_output: self.combined_output.clone(),
            duration_millis: self.duration.as_millis().min(u64::MAX as u128) as u64,
            error: self.error.clone(),
        }
    }
}

/// The disposition of one configured gate, including gates skipped after a failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifierGateDisposition {
    Passed,
    Failed,
    SpawnFailed,
    Interrupted,
    NotRun { blocked_by: usize },
}

/// One configured gate and its executed or structured not-run disposition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifierGateResult {
    pub command: String,
    pub disposition: VerifierGateDisposition,
    pub result: Option<VerifierCommandResult>,
}

/// Results from the ordered verifier command sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifierRun {
    pub commands: Vec<VerifierCommandResult>,
    pub gate_results: Vec<VerifierGateResult>,
    pub stopped_after_failure: bool,
    pub first_failed_gate: Option<usize>,
}

impl VerifierRun {
    pub fn all_commands_succeeded(&self) -> bool {
        !self.commands.is_empty() && self.commands.iter().all(VerifierCommandResult::is_success)
    }

    /// Build the serializable evidence retained by a procedure report.
    pub fn report(&self, eligibility: CandidateEligibility) -> VerifierReport {
        VerifierReport {
            gates: self
                .gate_results
                .iter()
                .map(|gate| VerifierGateEvidence {
                    command: gate.command.clone(),
                    disposition: gate.disposition.clone(),
                    result: gate.result.as_ref().map(VerifierCommandResult::evidence),
                })
                .collect(),
            stopped_after_failure: self.stopped_after_failure,
            first_failed_gate: self.first_failed_gate,
            eligibility,
        }
    }
}

/// Serializable evidence for one executed verifier command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifierCommandEvidence {
    pub command: String,
    pub disposition: VerifierCommandDisposition,
    pub success: bool,
    pub exit_code: Option<i32>,
    pub stdout: BoundedVerifierOutput,
    pub stderr: BoundedVerifierOutput,
    pub combined_output: BoundedVerifierOutput,
    pub duration_millis: u64,
    pub error: Option<String>,
}

/// Serializable verification evidence for one procedure report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifierReport {
    pub gates: Vec<VerifierGateEvidence>,
    pub stopped_after_failure: bool,
    pub first_failed_gate: Option<usize>,
    pub eligibility: CandidateEligibility,
}

/// Serializable evidence for an executed or skipped configured gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifierGateEvidence {
    pub command: String,
    pub disposition: VerifierGateDisposition,
    pub result: Option<VerifierCommandEvidence>,
}

/// Runs configured commands in the supplied disposable workspace.
#[derive(Debug, Clone)]
pub struct VerifierCommandRunner {
    interrupt: Arc<AtomicBool>,
}

impl Default for VerifierCommandRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl VerifierCommandRunner {
    pub fn new() -> Self {
        Self {
            interrupt: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn with_interrupt(interrupt: Arc<AtomicBool>) -> Self {
        Self { interrupt }
    }

    pub fn interrupt(&self) -> &Arc<AtomicBool> {
        &self.interrupt
    }

    /// Execute each command in order and stop after its first non-success.
    pub async fn run(&self, workspace: &Path, commands: &[String]) -> VerifierRun {
        let mut results = Vec::with_capacity(commands.len());
        let mut gate_results = Vec::with_capacity(commands.len());
        for (index, command) in commands.iter().enumerate() {
            let result = if self.interrupted() {
                interrupted_result(command)
            } else {
                run_one(workspace, command, &self.interrupt).await
            };
            let failed = !result.success;
            let disposition = gate_disposition(&result);
            gate_results.push(VerifierGateResult {
                command: command.clone(),
                disposition,
                result: Some(result.clone()),
            });
            results.push(result);
            if failed {
                gate_results.extend(commands[index + 1..].iter().map(|command| {
                    VerifierGateResult {
                        command: command.clone(),
                        disposition: VerifierGateDisposition::NotRun { blocked_by: index },
                        result: None,
                    }
                }));
                return VerifierRun {
                    commands: results,
                    gate_results,
                    stopped_after_failure: true,
                    first_failed_gate: Some(index),
                };
            }
        }
        VerifierRun {
            commands: results,
            gate_results,
            stopped_after_failure: false,
            first_failed_gate: None,
        }
    }

    fn interrupted(&self) -> bool {
        self.interrupt.load(Ordering::SeqCst)
    }
}

fn gate_disposition(result: &VerifierCommandResult) -> VerifierGateDisposition {
    match result.disposition {
        VerifierCommandDisposition::Passed => VerifierGateDisposition::Passed,
        VerifierCommandDisposition::Failed => VerifierGateDisposition::Failed,
        VerifierCommandDisposition::SpawnFailed => VerifierGateDisposition::SpawnFailed,
        VerifierCommandDisposition::Interrupted => VerifierGateDisposition::Interrupted,
    }
}

/// Run configured commands with a fresh interrupt flag.
pub async fn run_verifier_commands(workspace: &Path, commands: &[String]) -> VerifierRun {
    VerifierCommandRunner::new().run(workspace, commands).await
}

/// Run configured commands using the caller's shared interrupt flag.
pub async fn run_verifier_commands_with_interrupt(
    workspace: &Path,
    commands: &[String],
    interrupt: Arc<AtomicBool>,
) -> VerifierRun {
    VerifierCommandRunner::with_interrupt(interrupt)
        .run(workspace, commands)
        .await
}

async fn run_one(
    workspace: &Path,
    command_text: &str,
    interrupt: &Arc<AtomicBool>,
) -> VerifierCommandResult {
    let started = Instant::now();
    let arguments = match tokenize_command_line(command_text) {
        Ok(arguments) if !arguments.is_empty() => arguments,
        Ok(_) => {
            return failed_result(
                command_text,
                VerifierCommandDisposition::SpawnFailed,
                started.elapsed(),
                "command is empty".to_string(),
            );
        }
        Err(error) => {
            return failed_result(
                command_text,
                VerifierCommandDisposition::SpawnFailed,
                started.elapsed(),
                error,
            );
        }
    };

    let resolved = resolve_command(&arguments[0]);
    let mut command = Command::new(&resolved.program);
    #[cfg(windows)]
    if resolved.program.eq_ignore_ascii_case("cmd") && resolved.prefix_args.len() >= 2 {
        command.raw_arg(raw_cmd_command_line(
            &resolved.prefix_args[1],
            &arguments[1..],
        ));
    } else {
        command.args(&resolved.prefix_args).args(&arguments[1..]);
    }
    #[cfg(not(windows))]
    command.args(&resolved.prefix_args).args(&arguments[1..]);
    command
        .current_dir(workspace)
        .kill_on_drop(true)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    crate::process_group::prepare(&mut command);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return failed_result(
                command_text,
                VerifierCommandDisposition::SpawnFailed,
                started.elapsed(),
                error.to_string(),
            );
        }
    };
    crate::process_group::adopt(&child);

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let (stdout, stderr) = match (stdout, stderr) {
        (Some(stdout), Some(stderr)) => (stdout, stderr),
        _ => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return failed_result(
                command_text,
                VerifierCommandDisposition::SpawnFailed,
                started.elapsed(),
                "verifier child did not provide piped output".to_string(),
            );
        }
    };

    let output = collect_output(&mut child, stdout, stderr, interrupt).await;
    let duration = started.elapsed();
    let disposition = if output.interrupted {
        VerifierCommandDisposition::Interrupted
    } else if output
        .status
        .as_ref()
        .is_some_and(std::process::ExitStatus::success)
        && output.error.is_none()
    {
        VerifierCommandDisposition::Passed
    } else {
        VerifierCommandDisposition::Failed
    };
    let success = disposition == VerifierCommandDisposition::Passed;
    VerifierCommandResult {
        command: command_text.to_string(),
        disposition,
        success,
        exit_code: output.status.and_then(|status| status.code()),
        stdout: output.stdout.finish(),
        stderr: output.stderr.finish(),
        combined_output: output.combined.finish(),
        duration,
        error: output.error,
    }
}

#[cfg(windows)]
fn raw_cmd_command_line(batch_path: &str, arguments: &[String]) -> String {
    let mut command_line = format!("/c \"\"{batch_path}\"");
    for argument in arguments {
        command_line.push(' ');
        command_line.push('"');
        command_line.push_str(argument);
        command_line.push('"');
    }
    command_line.push('"');
    command_line
}

fn failed_result(
    command: &str,
    disposition: VerifierCommandDisposition,
    duration: Duration,
    error: String,
) -> VerifierCommandResult {
    VerifierCommandResult {
        command: command.to_string(),
        disposition,
        success: false,
        exit_code: None,
        stdout: BoundedVerifierOutput::empty(),
        stderr: BoundedVerifierOutput::empty(),
        combined_output: BoundedVerifierOutput::empty(),
        duration,
        error: Some(error),
    }
}

fn interrupted_result(command: &str) -> VerifierCommandResult {
    failed_result(
        command,
        VerifierCommandDisposition::Interrupted,
        Duration::ZERO,
        "verifier command was interrupted before it started".to_string(),
    )
}

struct CollectedOutput {
    status: Option<std::process::ExitStatus>,
    interrupted: bool,
    error: Option<String>,
    stdout: EdgeCapture,
    stderr: EdgeCapture,
    combined: EdgeCapture,
}

async fn collect_output(
    child: &mut Child,
    stdout: ChildStdout,
    stderr: ChildStderr,
    interrupt: &Arc<AtomicBool>,
) -> CollectedOutput {
    let (sender, mut receiver) = mpsc::channel(32);
    let stdout_task = tokio::spawn(read_stream(stdout, StreamKind::Stdout, sender.clone()));
    let stderr_task = tokio::spawn(read_stream(stderr, StreamKind::Stderr, sender));
    let mut status = None;
    let mut interrupted = false;
    let mut error = None;
    let mut closed_streams = 0_u8;
    let mut stdout_capture = EdgeCapture::default();
    let mut stderr_capture = EdgeCapture::default();
    let mut combined_capture = EdgeCapture::default();

    while !(interrupted && status.is_some()) && (status.is_none() || closed_streams < 2) {
        tokio::select! {
            message = receiver.recv(), if closed_streams < 2 => {
                match message {
                    Some(StreamMessage::Data(kind, bytes)) => {
                        combined_capture.push(&bytes);
                        match kind {
                            StreamKind::Stdout => stdout_capture.push(&bytes),
                            StreamKind::Stderr => stderr_capture.push(&bytes),
                        }
                    }
                    Some(StreamMessage::Closed(_kind)) => closed_streams += 1,
                    Some(StreamMessage::Error(kind, read_error)) => {
                        let prefix = match kind {
                            StreamKind::Stdout => "stdout",
                            StreamKind::Stderr => "stderr",
                        };
                        error.get_or_insert_with(|| format!("{prefix} read failed: {read_error}"));
                        closed_streams += 1;
                    }
                    None => closed_streams = 2,
                }
            }
            _ = tokio::time::sleep(INTERRUPT_POLL_INTERVAL), if status.is_none() => {
                if interrupt.load(Ordering::SeqCst) && !interrupted {
                    interrupted = true;
                    if let Err(kill_error) = crate::process_group::terminate(child) {
                        error.get_or_insert_with(|| format!("could not stop interrupted verifier: {kill_error}"));
                    }
                    break;
                }
                match child.try_wait() {
                    Ok(Some(exit_status)) => status = Some(exit_status),
                    Ok(None) => {}
                    Err(wait_error) => {
                        error.get_or_insert_with(|| wait_error.to_string());
                    }
                }
            }
        }
    }

    if !interrupted && status.is_none() {
        match child.wait().await {
            Ok(exit_status) => status = Some(exit_status),
            Err(wait_error) => {
                error.get_or_insert_with(|| wait_error.to_string());
            }
        }
    }

    if interrupted {
        stdout_task.abort();
        stderr_task.abort();
    } else {
        let _ = stdout_task.await;
        let _ = stderr_task.await;
    }
    CollectedOutput {
        status,
        interrupted,
        error,
        stdout: stdout_capture,
        stderr: stderr_capture,
        combined: combined_capture,
    }
}

#[derive(Debug, Clone, Copy)]
enum StreamKind {
    Stdout,
    Stderr,
}

enum StreamMessage {
    Data(StreamKind, Vec<u8>),
    Closed(StreamKind),
    Error(StreamKind, String),
}

async fn read_stream<R>(mut reader: R, kind: StreamKind, sender: mpsc::Sender<StreamMessage>)
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut buffer = vec![0; OUTPUT_CHUNK_BYTES];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) => {
                let _ = sender.send(StreamMessage::Closed(kind)).await;
                return;
            }
            Ok(length) => {
                if sender
                    .send(StreamMessage::Data(kind, buffer[..length].to_vec()))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Err(error) => {
                let _ = sender
                    .send(StreamMessage::Error(kind, error.to_string()))
                    .await;
                return;
            }
        }
    }
}

#[derive(Default)]
struct EdgeCapture {
    first: Vec<u8>,
    bounded: Vec<u8>,
    tail: VecDeque<u8>,
    total: u64,
}

impl EdgeCapture {
    fn push(&mut self, bytes: &[u8]) {
        self.total = self.total.saturating_add(bytes.len() as u64);
        let first_remaining = VERIFIER_OUTPUT_EDGE_BYTES.saturating_sub(self.first.len());
        self.first
            .extend_from_slice(&bytes[..bytes.len().min(first_remaining)]);
        if self.bounded.len() < OUTPUT_BUFFER_BYTES {
            let remaining = OUTPUT_BUFFER_BYTES - self.bounded.len();
            self.bounded
                .extend_from_slice(&bytes[..bytes.len().min(remaining)]);
        }
        for byte in bytes {
            self.tail.push_back(*byte);
            if self.tail.len() > VERIFIER_OUTPUT_EDGE_BYTES {
                self.tail.pop_front();
            }
        }
    }

    fn finish(self) -> BoundedVerifierOutput {
        let truncated = self.total > OUTPUT_BUFFER_BYTES as u64;
        let first_edge = String::from_utf8_lossy(&self.first).into_owned();
        let last_edge =
            String::from_utf8_lossy(&self.tail.iter().copied().collect::<Vec<_>>()).into_owned();
        let bytes = if truncated {
            let mut bytes = self.first;
            bytes.extend_from_slice(b"\n...[output truncated]...\n");
            bytes.extend(self.tail);
            bytes
        } else {
            self.bounded
        };
        BoundedVerifierOutput {
            text: String::from_utf8_lossy(&bytes).into_owned(),
            first_edge,
            last_edge,
            truncated,
            bytes_seen: self.total,
        }
    }
}

fn tokenize_command_line(command: &str) -> Result<Vec<String>, String> {
    let mut arguments = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut had_content = false;
    for character in command.chars() {
        match (quote, character) {
            (None, '"' | '\'') => quote = Some(character),
            (Some(active), character) if character == active => quote = None,
            (None, character) if character.is_whitespace() => {
                if had_content {
                    arguments.push(std::mem::take(&mut current));
                    had_content = false;
                }
            }
            (_, character) => {
                current.push(character);
                had_content = true;
            }
        }
    }
    if quote.is_some() {
        return Err("command has an unterminated quote".to_string());
    }
    if had_content {
        arguments.push(current);
    }
    Ok(arguments)
}

/// The reason a candidate cannot be promoted after verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateIneligibility {
    PatchCheckFailed,
    PatchApplyFailed,
    NoVerifierCommands,
    VerifierCommandFailed {
        index: usize,
        command: String,
        disposition: VerifierCommandDisposition,
    },
}

/// Deterministic eligibility from patch and command results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateEligibility {
    pub eligible: bool,
    pub reason: Option<CandidateIneligibility>,
}

impl CandidateEligibility {
    pub fn eligible() -> Self {
        Self {
            eligible: true,
            reason: None,
        }
    }

    pub fn ineligible(reason: CandidateIneligibility) -> Self {
        Self {
            eligible: false,
            reason: Some(reason),
        }
    }
}

/// Evaluate whether every deterministic verification gate succeeded.
pub fn evaluate_candidate_eligibility(
    patch_check: &GitApplyResult,
    patch_apply: &GitApplyResult,
    commands: &[VerifierCommandResult],
) -> CandidateEligibility {
    if !patch_check.success {
        return CandidateEligibility::ineligible(CandidateIneligibility::PatchCheckFailed);
    }
    if !patch_apply.success {
        return CandidateEligibility::ineligible(CandidateIneligibility::PatchApplyFailed);
    }
    if commands.is_empty() {
        return CandidateEligibility::ineligible(CandidateIneligibility::NoVerifierCommands);
    }
    if let Some((index, result)) = commands
        .iter()
        .enumerate()
        .find(|(_, result)| !result.success)
    {
        return CandidateEligibility::ineligible(CandidateIneligibility::VerifierCommandFailed {
            index,
            command: result.command.clone(),
            disposition: result.disposition,
        });
    }
    CandidateEligibility::eligible()
}

/// Evaluate a completed isolated patch workspace and its verifier run.
pub fn evaluate_applied_patch_eligibility(
    applied: &AppliedPatchWorkspace,
    verifier_run: &VerifierRun,
) -> CandidateEligibility {
    evaluate_candidate_eligibility(
        applied.check_result(),
        applied.apply_result(),
        &verifier_run.commands,
    )
}
