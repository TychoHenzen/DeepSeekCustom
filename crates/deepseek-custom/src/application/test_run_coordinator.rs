//! Ownership of one active test run and its terminal results.

use std::{collections::VecDeque, io, path::Path};

use super::test_control::{
    ActiveTestSlot, RetainedTestResult, TEST_OUTPUT_LIMIT_BYTES, TestClock, TestControlSnapshot,
    TestCounts, TestDiscoveryState, TestExecution, TestExecutionError, TestExecutor,
    TestOutputChunk, TestProcessExit, TestResultStore, TestRunRequest, TestRunRequestError,
    classify_test_outcome, parse_cargo_test_report,
};

/// Owns the active identity, executor lifecycle, cancellation, polling, and
/// retained terminal result for the process-wide test slot.
pub struct TestRunCoordinator {
    pub active: Option<ActiveTestSlot>,
    pub latest_result: Option<RetainedTestResult>,
    execution: Option<Box<dyn TestExecution>>,
    next_run_number: u64,
    output_buffer: Option<BoundedTestOutput>,
}

impl Default for TestRunCoordinator {
    fn default() -> Self {
        Self {
            active: None,
            latest_result: None,
            execution: None,
            next_run_number: 1,
            output_buffer: None,
        }
    }
}

impl TestRunCoordinator {
    pub fn snapshot(&self, discovery: &TestDiscoveryState) -> TestControlSnapshot {
        TestControlSnapshot {
            discovery: discovery.clone(),
            active: self.active.clone(),
            latest_result: self.latest_result.clone(),
            retained_results: Vec::new(),
            retained_result_warnings: Vec::new(),
        }
    }

    pub fn snapshot_with_results(
        &self,
        discovery: &TestDiscoveryState,
        store: &TestResultStore,
    ) -> io::Result<TestControlSnapshot> {
        let loaded = store.load_recent()?;
        Ok(TestControlSnapshot {
            discovery: discovery.clone(),
            active: self.active.clone(),
            latest_result: self.latest_result.clone(),
            retained_results: loaded.results,
            retained_result_warnings: loaded.warnings,
        })
    }

    pub fn retain_latest(&self, store: &TestResultStore) -> io::Result<()> {
        let result = self.latest_result.as_ref().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "no terminal test result to retain")
        })?;
        store.store(result)
    }

    pub fn start(
        &mut self,
        discovery: &TestDiscoveryState,
        request: &TestRunRequest,
        project_root: &Path,
        executor: &dyn TestExecutor,
        clock: &dyn TestClock,
    ) -> Result<&ActiveTestSlot, TestRunRequestError> {
        if let Some(active) = &self.active {
            return Err(TestRunRequestError::ActiveRun {
                active_run_id: active.run_id.clone(),
            });
        }
        let (invocation, execution) = discovery.start_run(request, project_root, executor)?;
        let started_at_ms = clock.now_ms();
        let run_id = format!("test-run-{}-{}", started_at_ms, self.next_run_number);
        self.next_run_number = self.next_run_number.saturating_add(1);
        self.active = Some(ActiveTestSlot {
            run_id,
            identity: request.identity.clone(),
            command: std::iter::once(invocation.program.clone())
                .chain(invocation.args.iter().cloned())
                .collect(),
            working_dir: invocation.working_dir,
            started_at_ms,
            elapsed_ms: 0,
            running: true,
            counts: TestCounts::default(),
            output_chunks: Vec::new(),
            output: String::new(),
            omitted_output_bytes: 0,
        });
        self.execution = Some(execution);
        self.output_buffer = Some(BoundedTestOutput::default());
        Ok(self
            .active
            .as_ref()
            .expect("active test slot was just stored"))
    }

    pub fn poll(
        &mut self,
        clock: &dyn TestClock,
    ) -> Result<Option<&RetainedTestResult>, TestExecutionError> {
        let Some(active) = &mut self.active else {
            return Ok(self.latest_result.as_ref());
        };
        let execution = self.execution.as_mut().expect("active run owns execution");
        collect_output(active, execution.as_mut(), self.output_buffer.as_mut())?;
        let now_ms = clock.now_ms();
        active.elapsed_ms = now_ms.saturating_sub(active.started_at_ms);
        let Some(exit) = execution.try_wait()? else {
            return Ok(None);
        };
        let result = finish_active(active, exit, now_ms, false);
        self.active = None;
        self.execution = None;
        self.output_buffer = None;
        self.latest_result = Some(result);
        Ok(self.latest_result.as_ref())
    }

    pub fn cancel(
        &mut self,
        clock: &dyn TestClock,
    ) -> Result<Option<&RetainedTestResult>, TestExecutionError> {
        let Some(mut active) = self.active.take() else {
            return Ok(self.latest_result.as_ref());
        };
        let mut execution = self.execution.take().expect("active run owns execution");
        collect_output(&mut active, execution.as_mut(), self.output_buffer.as_mut())?;
        let exit = execution.cancel_and_wait()?;
        let result = finish_active(&active, exit, clock.now_ms(), true);
        self.output_buffer = None;
        self.latest_result = Some(result);
        Ok(self.latest_result.as_ref())
    }
}

fn collect_output(
    active: &mut ActiveTestSlot,
    execution: &mut dyn TestExecution,
    output_buffer: Option<&mut BoundedTestOutput>,
) -> Result<(), TestExecutionError> {
    let output_buffer = output_buffer.expect("active run owns an output buffer");
    let mut chunks = Vec::new();
    while let Some(chunk) = execution.next_output()? {
        chunks.push(chunk);
    }
    chunks.sort_by_key(|chunk| chunk.sequence);
    for chunk in chunks {
        output_buffer.push(chunk.text.as_bytes());
        let rendered = output_buffer.render();
        active.output = rendered.text;
        active.omitted_output_bytes = rendered.omitted_bytes;
        active.output_chunks = vec![TestOutputChunk {
            sequence: chunk.sequence,
            stream: chunk.stream,
            text: active.output.clone(),
        }];
    }
    active.counts = parse_cargo_test_report(&active.output).0;
    Ok(())
}

fn finish_active(
    active: &ActiveTestSlot,
    exit: TestProcessExit,
    finished_at_ms: u64,
    cancelled: bool,
) -> RetainedTestResult {
    let (counts, failed_tests, cargo_reported_result) = parse_cargo_test_report(&active.output);
    RetainedTestResult {
        run_id: active.run_id.clone(),
        identity: active.identity.clone(),
        command: active.command.clone(),
        working_dir: active.working_dir.clone(),
        started_at_ms: active.started_at_ms,
        duration_ms: finished_at_ms.saturating_sub(active.started_at_ms),
        outcome: classify_test_outcome(exit, cargo_reported_result, cancelled),
        counts,
        exit_code: exit.exit_code,
        failed_tests,
        output: active.output.clone(),
        omitted_output_bytes: active.omitted_output_bytes,
    }
}

#[derive(Default)]
struct BoundedTestOutput {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    total_bytes: u64,
}

struct RenderedTestOutput {
    text: String,
    omitted_bytes: u64,
}

impl BoundedTestOutput {
    fn push(&mut self, bytes: &[u8]) {
        const MARKER_RESERVE: usize = 96;
        let side_limit = (TEST_OUTPUT_LIMIT_BYTES - MARKER_RESERVE) / 2;
        self.total_bytes = self.total_bytes.saturating_add(bytes.len() as u64);
        let head_count = side_limit.saturating_sub(self.head.len()).min(bytes.len());
        self.head.extend_from_slice(&bytes[..head_count]);
        for byte in &bytes[head_count..] {
            if self.tail.len() == side_limit {
                self.tail.pop_front();
            }
            self.tail.push_back(*byte);
        }
    }

    fn render(&self) -> RenderedTestOutput {
        let retained = self.head.len() + self.tail.len();
        let omitted = self.total_bytes.saturating_sub(retained as u64);
        if omitted == 0 {
            let mut bytes = self.head.clone();
            bytes.extend(self.tail.iter());
            return RenderedTestOutput {
                text: String::from_utf8_lossy(&bytes).into_owned(),
                omitted_bytes: 0,
            };
        }
        let mut text = String::from_utf8_lossy(&self.head).into_owned();
        text.push_str(&format!(
            "\n...[{omitted} bytes omitted by 4 MiB output limit]...\n"
        ));
        text.push_str(&String::from_utf8_lossy(
            &self.tail.iter().copied().collect::<Vec<_>>(),
        ));
        RenderedTestOutput {
            text,
            omitted_bytes: omitted,
        }
    }
}
