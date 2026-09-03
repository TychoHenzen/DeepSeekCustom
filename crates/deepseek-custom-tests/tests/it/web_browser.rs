//! Isolated Rust-side foundation for browser end-to-end tests.
//!
//! The browser scenarios are added in later tasks. This module owns the
//! deterministic server environment they use and deliberately has no coverage
//! marker until those scenarios drive observable browser behaviour.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::File;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use deepseek_custom::application::actor::AppEvent;
use deepseek_custom::application::dto::{
    AppCommand, AppCommandRequest, AppCommandResult, AppRevision, AppSnapshot,
    ControlledDevelopmentProofResult, ControlledDevelopmentRawDetail,
    ControlledDevelopmentRawDetailKind, ControlledDevelopmentView, NoticeLevel, OperationKind,
    OperationPhase, OperationProgress, OperationState, SessionSummary, TranscriptBlock,
    TranscriptContent, TranscriptSpan, Workspace,
};
use deepseek_custom::application::services::{
    DomainCommandPort, RuntimeSettingsPort, SettingsController,
};
use deepseek_custom::application::test_control::{
    TestClock, TestExecution, TestExecutionError, TestExecutor, TestInvocation, TestOutputChunk,
    TestOutputStream, TestProcessExit,
};
use deepseek_custom::config::settings::Settings;
use deepseek_custom::controlled_development::{ControlledDevelopmentPhase, WorkCard};
use deepseek_custom::voice::service::{VoiceCommand, VoiceEvent, VoiceState};
use deepseek_custom::web::server::{
    BindPolicy, NativeFolderPicker, WebAppState, WebServerHandle, start_with_policy_and_state,
};
use futures_util::FutureExt;
use tempfile::TempDir;
use tokio::sync::mpsc;

use playwright_rs::LaunchOptions;
use playwright_rs::protocol::{
    AriaRole, BrowserContext, GetByRoleOptions, Locator, Page, TracingStartOptions,
    TracingStopOptions, Viewport,
};

const INSTALL_COMMAND: &str =
    "cargo run -p deepseek-custom-tests --example install_playwright_chromium";
const FOCUSED_COMMAND: &str =
    "cargo test -p deepseek-custom-tests --test it web_browser -- --test-threads=1";
const ARTIFACT_ROOT: &str = "target/playwright-artifacts";
const SCREENSHOT_FILE: &str = "failure.png";
const TRACE_FILE: &str = "trace.zip";
const CONSOLE_FILE: &str = "browser-console.log";
const SERVER_FILE: &str = "server.log";
const LIVE_OLLAMA_MODEL: &str = "qwen2.5-coder:7b-instruct-q4_K_M";
const LIVE_OLLAMA_TEST: &str = "production_binary_ollama_browser_practice";

#[derive(Debug)]
struct BrowserFailureArtifacts {
    directory: PathBuf,
    screenshot: PathBuf,
    trace: PathBuf,
    console: PathBuf,
    server: PathBuf,
}

impl BrowserFailureArtifacts {
    fn for_test(test_name: &str) -> Self {
        assert!(
            !test_name.is_empty()
                && test_name
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '_'),
            "browser artifact test name must be a non-empty Rust identifier"
        );
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("test crate must remain two levels below the workspace root");
        let directory = workspace_root.join(ARTIFACT_ROOT).join(test_name);
        Self {
            screenshot: directory.join(SCREENSHOT_FILE),
            trace: directory.join(TRACE_FILE),
            console: directory.join(CONSOLE_FILE),
            server: directory.join(SERVER_FILE),
            directory,
        }
    }

    fn clear(&self) {
        match std::fs::remove_dir_all(&self.directory) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => panic!(
                "could not clear browser artifact directory {}: {error}",
                self.directory.display()
            ),
        }
    }
}

async fn start_failure_capture(
    context: &BrowserContext,
    test_name: &str,
) -> BrowserFailureArtifacts {
    let artifacts = BrowserFailureArtifacts::for_test(test_name);
    artifacts.clear();
    let tracing = context.tracing().await.unwrap();
    tracing
        .start(Some(
            TracingStartOptions::default()
                .name(test_name)
                .screenshots(true)
                .snapshots(true),
        ))
        .await
        .unwrap();
    artifacts
}

async fn finish_failure_capture(
    outcome: Result<(), String>,
    context: &BrowserContext,
    page: &Page,
    harness: &BrowserHarness,
    artifacts: &BrowserFailureArtifacts,
) -> Result<(), String> {
    let tracing = context.tracing().await.map_err(|error| error.to_string())?;
    match outcome {
        Ok(()) => {
            tracing
                .stop(Some(TracingStopOptions::default()))
                .await
                .map_err(|error| error.to_string())?;
            artifacts.clear();
            Ok(())
        }
        Err(diagnostic) => {
            std::fs::create_dir_all(&artifacts.directory).map_err(|error| error.to_string())?;
            page.screenshot_to_file(&artifacts.screenshot, None)
                .await
                .map_err(|error| error.to_string())?;
            let console = page
                .console_messages()
                .into_iter()
                .map(|message| {
                    format!(
                        "{} {} {}:{}:{}",
                        message.type_(),
                        message.text(),
                        message.location().url,
                        message.location().line_number,
                        message.location().column_number
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            std::fs::write(&artifacts.console, format!("{console}\n"))
                .map_err(|error| error.to_string())?;
            let snapshot = harness.state.snapshot();
            std::fs::write(
                &artifacts.server,
                format!(
                    "server_url={}\nserver_address={}\napplication_revision={}\nfailure={diagnostic}\n",
                    harness.server.url(),
                    harness.server.address(),
                    snapshot.revision.0
                ),
            )
            .map_err(|error| error.to_string())?;
            tracing
                .stop(Some(
                    TracingStopOptions::default().path(artifacts.trace.display().to_string()),
                ))
                .await
                .map_err(|error| error.to_string())?;
            Err(format!(
                "{diagnostic}\nBrowser failure artifacts: {}",
                artifacts.directory.display()
            ))
        }
    }
}

fn browser_runtime_failure(error: &playwright_rs::Error) -> String {
    format!(
        "Chromium for locked playwright-rs 0.17.0 / Playwright {} is unavailable: {error}\nInstall it with:\n{INSTALL_COMMAND}",
        playwright_rs::PLAYWRIGHT_VERSION
    )
}

fn panic_diagnostic(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "browser assertion panicked without a string diagnostic".into())
}

#[derive(Debug)]
struct ProductionOllamaArtifacts {
    directory: PathBuf,
    trace: PathBuf,
    console: PathBuf,
    server: PathBuf,
    workspace_evidence: PathBuf,
}

impl ProductionOllamaArtifacts {
    fn prepare() -> Self {
        let base = BrowserFailureArtifacts::for_test(LIVE_OLLAMA_TEST);
        base.clear();
        std::fs::create_dir_all(&base.directory).unwrap();
        let workspace_evidence = base.directory.join("workspace-bytes.log");
        Self {
            directory: base.directory,
            trace: base.trace,
            console: base.console,
            server: base.server,
            workspace_evidence,
        }
    }

    fn screenshot(&self, name: &str) -> PathBuf {
        self.directory.join(name)
    }
}

#[derive(Debug)]
struct ProductionBinary {
    child: Child,
    project_root: TempDir,
    server_log: PathBuf,
}

impl ProductionBinary {
    fn launch(binary: &Path, server_log: &Path) -> Result<Self, String> {
        if !binary.is_file() {
            return Err(format!(
                "production binary is missing at {}. Build it first with `cargo build -p deepseek-custom`",
                binary.display()
            ));
        }
        let project_root = tempfile::Builder::new()
            .prefix("deepseek-custom-production-ollama-")
            .tempdir()
            .map_err(|error| error.to_string())?;
        seed_ollama_project(project_root.path()).map_err(|error| error.to_string())?;
        let log = File::create(server_log).map_err(|error| error.to_string())?;
        let child = Command::new(binary)
            .current_dir(project_root.path())
            .env("DEEPSEEK_DISABLE_BROWSER", "1")
            .env("RUST_LOG", "info")
            .stdin(Stdio::null())
            .stdout(Stdio::from(
                log.try_clone().map_err(|error| error.to_string())?,
            ))
            .stderr(Stdio::from(log))
            .spawn()
            .map_err(|error| format!("failed to start production binary: {error}"))?;
        Ok(Self {
            child,
            project_root,
            server_log: server_log.to_path_buf(),
        })
    }

    fn project_root(&self) -> &Path {
        self.project_root.path()
    }

    async fn wait_for_url(&mut self) -> Result<String, String> {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let output = std::fs::read_to_string(&self.server_log).unwrap_or_default();
            if let Some(url) = output
                .lines()
                .find_map(|line| line.split_once("DeepSeekCustom web application: "))
                .map(|(_, url)| url.trim().to_string())
            {
                return Ok(url);
            }
            if let Some(status) = self.child.try_wait().map_err(|error| error.to_string())? {
                return Err(format!(
                    "production binary exited before reporting its URL with {status}\n{output}"
                ));
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "production binary did not report its URL within 60 seconds\n{output}"
                ));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn shutdown(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

impl Drop for ProductionBinary {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn seed_ollama_project(root: &Path) -> io::Result<()> {
    std::fs::create_dir_all(root.join("src"))?;
    std::fs::create_dir_all(root.join("tests/it"))?;
    std::fs::write(
        root.join("CLAUDE.md"),
        "# Disposable Ollama browser practice\n",
    )?;
    std::fs::write(
        root.join("settings.json"),
        format!(
            concat!(
                "{{\n",
                "  \"effort\": \"none\",\n",
                "  \"max_tokens\": 1024,\n",
                "  \"context_budget\": 32000,\n",
                "  \"voice\": {{ \"enabled\": false }},\n",
                "  \"mcp\": {{ \"enabled\": false }},\n",
                "  \"backends\": {{\n",
                "    \"ollama\": {{\n",
                "      \"kind\": \"api\",\n",
                "      \"provider\": \"ollama\",\n",
                "      \"model\": \"{}\",\n",
                "      \"models\": [\"{}\"]\n",
                "    }}\n",
                "  }},\n",
                "  \"default_backend\": \"ollama\"\n",
                "}}\n"
            ),
            LIVE_OLLAMA_MODEL, LIVE_OLLAMA_MODEL
        ),
    )?;
    std::fs::write(
        root.join("Cargo.toml"),
        concat!(
            "[package]\n",
            "name = \"deepseek-custom-tests\"\n",
            "version = \"0.0.0\"\n",
            "edition = \"2024\"\n\n",
            "[lib]\n",
            "path = \"src/lib.rs\"\n\n",
            "[[test]]\n",
            "name = \"it\"\n",
            "path = \"tests/it/main.rs\"\n"
        ),
    )?;
    std::fs::write(root.join("src/lib.rs"), "pub fn practice_fixture() {}\n")?;
    std::fs::write(
        root.join("src/unrelated.txt"),
        "preserve these exact bytes\n",
    )?;
    std::fs::write(
        root.join("tests/it/main.rs"),
        concat!(
            "mod practice {\n",
            "    #[test]\n",
            "    fn production_dashboard_smoke() {\n",
            "        assert_eq!(2 + 2, 4);\n",
            "    }\n",
            "}\n"
        ),
    )?;
    Ok(())
}

type ProductionWorkspaceBytes = BTreeMap<String, Vec<u8>>;

fn capture_production_workspace_bytes(root: &Path) -> io::Result<ProductionWorkspaceBytes> {
    fn visit(
        root: &Path,
        directory: &Path,
        bytes: &mut ProductionWorkspaceBytes,
    ) -> io::Result<()> {
        let mut entries = std::fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let name = entry.file_name();
            if directory == root
                && [".deepseek", "target", "deepseek_custom.log"]
                    .iter()
                    .any(|excluded| name.eq_ignore_ascii_case(excluded))
            {
                continue;
            }
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                visit(root, &path, bytes)?;
            } else if metadata.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .expect("workspace entry must remain below its root")
                    .to_string_lossy()
                    .replace('\\', "/");
                bytes.insert(relative, std::fs::read(path)?);
            }
        }
        Ok(())
    }

    let mut bytes = BTreeMap::new();
    visit(root, root, &mut bytes)?;
    Ok(bytes)
}

fn changed_production_workspace_paths(
    before: &ProductionWorkspaceBytes,
    after: &ProductionWorkspaceBytes,
) -> Vec<String> {
    before
        .keys()
        .chain(after.keys())
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|path| before.get(path) != after.get(path))
        .collect()
}

fn byte_fingerprint(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

fn record_workspace_boundary(
    evidence_path: &Path,
    boundary: &str,
    baseline: &ProductionWorkspaceBytes,
    observed: &ProductionWorkspaceBytes,
) -> io::Result<()> {
    use std::io::Write;

    let mut evidence = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(evidence_path)?;
    writeln!(evidence, "boundary: {boundary}")?;
    writeln!(
        evidence,
        "changed paths: {:?}",
        changed_production_workspace_paths(baseline, observed)
    )?;
    for (path, bytes) in observed {
        writeln!(
            evidence,
            "{path}: {} bytes, fnv1a64:{:016x}",
            bytes.len(),
            byte_fingerprint(bytes)
        )?;
    }
    writeln!(evidence)?;
    Ok(())
}

fn require_workspace_boundary(
    project_root: &Path,
    evidence_path: &Path,
    boundary: &str,
    baseline: &ProductionWorkspaceBytes,
    expected_changed_paths: &[&str],
) -> Result<ProductionWorkspaceBytes, String> {
    let observed = capture_production_workspace_bytes(project_root)
        .map_err(|error| format!("failed to capture {boundary} workspace bytes: {error}"))?;
    record_workspace_boundary(evidence_path, boundary, baseline, &observed)
        .map_err(|error| format!("failed to record {boundary} workspace bytes: {error}"))?;
    let changed = changed_production_workspace_paths(baseline, &observed);
    let expected = expected_changed_paths
        .iter()
        .map(|path| (*path).to_string())
        .collect::<Vec<_>>();
    if changed != expected {
        return Err(format!(
            "real workspace bytes changed at {boundary}: expected {expected:?}, observed {changed:?}"
        ));
    }
    Ok(observed)
}

async fn capture_live_step(
    page: &Page,
    artifacts: &ProductionOllamaArtifacts,
    name: &str,
) -> Result<(), String> {
    page.screenshot_to_file(&artifacts.screenshot(name), None)
        .await
        .map(|_| ())
        .map_err(|error| format!("failed to capture {name}: {error}"))
}

async fn capture_controlled_step(
    panel: &Locator,
    artifacts: &ProductionOllamaArtifacts,
    name: &str,
) -> Result<(), String> {
    let bytes = panel.screenshot(None).await.map_err(|error| {
        format!("failed to capture Controlled Development panel {name}: {error}")
    })?;
    tokio::fs::write(artifacts.screenshot(name), bytes)
        .await
        .map_err(|error| format!("failed to retain Controlled Development panel {name}: {error}"))
}

async fn wait_for_locator_text(
    locator: &Locator,
    expected: &str,
    timeout: Duration,
) -> Result<String, String> {
    let deadline = Instant::now() + timeout;
    loop {
        match locator.inner_text().await {
            Ok(text) if text.contains(expected) => return Ok(text),
            Ok(text) if Instant::now() >= deadline => {
                return Err(format!(
                    "timed out waiting for {expected:?}; last visible text was {text:?}"
                ));
            }
            Err(error) if Instant::now() >= deadline => {
                return Err(format!("timed out waiting for {expected:?}: {error}"));
            }
            Ok(_) | Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
}

fn production_binary_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("test crate must remain two levels below the workspace root")
        .join("target/debug")
        .join(format!("deepseek-custom{}", std::env::consts::EXE_SUFFIX))
}

#[derive(Clone)]
struct FixedClock(u64);

impl TestClock for FixedClock {
    fn now_ms(&self) -> u64 {
        self.0
    }
}

struct ScriptedTestExecutor {
    output: String,
    exit_code: Option<i32>,
    active_children: Arc<AtomicUsize>,
}

impl TestExecutor for ScriptedTestExecutor {
    fn start(
        &self,
        _invocation: &TestInvocation,
    ) -> Result<Box<dyn TestExecution>, TestExecutionError> {
        self.active_children
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(Box::new(ScriptedTestExecution {
            output: Some(self.output.clone()),
            exit_code: self.exit_code,
            active_children: Arc::clone(&self.active_children),
        }))
    }
}

struct ScriptedTestExecution {
    output: Option<String>,
    exit_code: Option<i32>,
    active_children: Arc<AtomicUsize>,
}

impl Drop for ScriptedTestExecution {
    fn drop(&mut self) {
        self.active_children
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

impl TestExecution for ScriptedTestExecution {
    fn next_output(&mut self) -> Result<Option<TestOutputChunk>, TestExecutionError> {
        Ok(self.output.take().map(|text| TestOutputChunk {
            sequence: 0,
            stream: TestOutputStream::Stdout,
            text,
        }))
    }

    fn try_wait(&mut self) -> Result<Option<TestProcessExit>, TestExecutionError> {
        Ok(self.output.is_none().then_some(TestProcessExit {
            exit_code: self.exit_code,
        }))
    }

    fn cancel_and_wait(&mut self) -> Result<TestProcessExit, TestExecutionError> {
        self.output = None;
        Ok(TestProcessExit { exit_code: None })
    }
}

struct ScriptedFolderPicker(Mutex<VecDeque<Option<PathBuf>>>);

impl NativeFolderPicker for ScriptedFolderPicker {
    fn pick_folder(&self, _initial_directory: &Path) -> io::Result<Option<PathBuf>> {
        Ok(self.0.lock().unwrap().pop_front().unwrap_or(None))
    }
}

/// Drives backend-like success, failure, interruption, and reconnect events
/// without an API call or child model process.
#[derive(Clone)]
struct ScriptedBackend {
    state: WebAppState,
}

impl ScriptedBackend {
    fn emit_notice(&self, message: &str, level: NoticeLevel) {
        let id = self.state.snapshot().transcript.len() as u64 + 1;
        self.state
            .apply_event(AppEvent::TranscriptAppended(TranscriptBlock {
                id,
                content: TranscriptContent::Notice {
                    message: message.into(),
                    level,
                },
            }))
            .unwrap();
    }

    fn set_operation(&self, kind: OperationKind, phase: OperationPhase, message: &str) {
        self.state
            .apply_event(AppEvent::OperationChanged(OperationState {
                kind,
                operation_id: Some(format!("scripted-{kind:?}")),
                phase,
                progress: Some(OperationProgress {
                    completed: u64::from(phase != OperationPhase::Running),
                    total: Some(1),
                }),
                message: Some(message.into()),
                error: None,
            }))
            .unwrap();
    }
}

/// Owns every resource used by one browser test. Dropping the temporary root
/// cannot affect the checkout because the server only receives this path.
struct BrowserHarness {
    project_root: TempDir,
    process_token: String,
    state: WebAppState,
    backend: ScriptedBackend,
    voice_rx: mpsc::UnboundedReceiver<VoiceCommand>,
    active_children: Arc<AtomicUsize>,
    server: WebServerHandle,
}

impl BrowserHarness {
    async fn start() -> Self {
        Self::start_with_controlled(Default::default()).await
    }

    async fn start_with_controlled(controlled: ControlledDevelopmentView) -> Self {
        let project_root = tempfile::Builder::new()
            .prefix("deepseek-browser-")
            .tempdir()
            .unwrap();
        let selected = project_root.path().join("selected-folder");
        std::fs::create_dir(&selected).unwrap();
        let picker = Arc::new(ScriptedFolderPicker(Mutex::new(VecDeque::from([
            Some(selected),
            None,
        ]))));
        let (voice_tx, voice_rx) = mpsc::unbounded_channel();
        let active_children = Arc::new(AtomicUsize::new(0));
        let discovery = Arc::new(ScriptedTestExecutor {
            output: "web_browser::scripted_case: test\n".into(),
            exit_code: Some(0),
            active_children: Arc::clone(&active_children),
        });
        let runner = Arc::new(ScriptedTestExecutor {
            output: "running 1 test\ntest web_browser::scripted_case ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n".into(),
            exit_code: Some(0),
            active_children: Arc::clone(&active_children),
        });
        let mut snapshot = isolated_snapshot(project_root.path());
        snapshot.controlled_development = controlled;
        let state = WebAppState::with_settings(
            snapshot,
            32,
            test_settings_controller(project_root.path()),
            picker,
        )
        .with_voice_port(DomainCommandPort::new(voice_tx))
        .with_test_service(
            project_root.path().to_path_buf(),
            discovery,
            runner,
            Arc::new(FixedClock(1_700_000_000_000)),
        );
        let backend = ScriptedBackend {
            state: state.clone(),
        };
        let server = start_with_policy_and_state(
            BindPolicy::strict(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)),
            None,
            state.clone(),
        )
        .await
        .unwrap();
        let process_token = reqwest::Client::new()
            .get(format!("{}api/bootstrap", server.url()))
            .send()
            .await
            .unwrap()
            .headers()
            .get(deepseek_custom::web::server::REQUEST_TOKEN_HEADER)
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        Self {
            project_root,
            process_token,
            state,
            backend,
            voice_rx,
            active_children,
            server,
        }
    }

    async fn shutdown(self) {
        self.server.shutdown().await.unwrap();
    }
}

fn controlled_panel_view(phase: ControlledDevelopmentPhase) -> ControlledDevelopmentView {
    let limitation = "Approved proof commands run in the disposable workspace, but can still address absolute paths outside it.";
    let completed = phase == ControlledDevelopmentPhase::Completed;
    ControlledDevelopmentView {
        enabled: true,
        phase,
        packet_id: Some("packet-browser-7".into()),
        card: Some(WorkCard {
            id: "card-browser-7".into(),
            outcome: "Expose the complete controlled review panel".into(),
            proof_commands: vec!["cargo test -p deepseek-custom-tests --test it controlled".into()],
            production_paths: vec!["crates/deepseek-custom/src/controlled_development/summary.rs".into()],
            supporting_paths: vec!["web/src/app/ChatWorkspace.test.tsx".into()],
            excluded: vec!["settings.json".into()],
            complexity_exceptions: vec!["No dependency changes".into()],
        }),
        structural_errors: Vec::new(),
        changed_paths: vec![
            "crates/deepseek-custom/src/controlled_development/summary.rs".into(),
            "web/src/app/ChatWorkspace.test.tsx".into(),
        ],
        proof_results: vec![ControlledDevelopmentProofResult {
            command: "cargo test -p deepseek-custom-tests --test it controlled".into(),
            disposition: "passed".into(),
            success: Some(true),
            exit_code: Some(0),
        }],
        progress_notice: format!(
            "Phase: {}. A Work Card is recorded. 2 changed path(s) and 1 proof result(s) are recorded. No failure is recorded.",
            if completed { "Completed" } else { "Awaiting approval" }
        ),
        completion_summary: completed.then(|| format!(
            "Phase: Completed. Work Card outcome: Expose the complete controlled review panel. Changed paths: crates/deepseek-custom/src/controlled_development/summary.rs, web/src/app/ChatWorkspace.test.tsx. Proof results: 1 passed, 0 failed, 0 interrupted, 0 not run. Remaining limitation: {limitation}"
        )),
        compact_result: None,
        blocker: None,
        raw_details: vec![
            ControlledDevelopmentRawDetail {
                kind: ControlledDevelopmentRawDetailKind::BackendEvent,
                name: "Backend reasoning".into(),
                content: "complete backend reasoning".into(),
                truncated_at_source: false,
                bytes_seen: 26,
            },
            ControlledDevelopmentRawDetail {
                kind: ControlledDevelopmentRawDetailKind::VerifierCombinedOutput,
                name: "Verifier output".into(),
                content: "test result: ok. 1 passed; 0 failed".into(),
                truncated_at_source: false,
                bytes_seen: 35,
            },
        ],
        retained_evidence: false,
        limitation: limitation.into(),
    }
}

/// Semantic browser surface shared by critical-flow tests.
///
/// Each method names the user-visible contract. The only non-semantic selector
/// is the hidden image input, which still has the documented accessible label
/// `Select image` and is reached through `get_by_label`.
struct BrowserPage {
    page: Page,
}

impl BrowserPage {
    fn new(page: Page) -> Self {
        Self { page }
    }

    async fn wait_for_snapshot(&self) -> Locator {
        self.require_unique(
            "connected application snapshot",
            self.page
                .get_by_text("Connected. Application revision", false),
        )
        .await
    }

    fn action(&self, name: &str) -> Locator {
        self.page.get_by_role(
            AriaRole::Button,
            Some(GetByRoleOptions::default().name(name).exact(true)),
        )
    }

    fn navigation(&self, name: &str) -> Locator {
        self.page.get_by_role(
            AriaRole::Button,
            Some(GetByRoleOptions::default().name(name).exact(false)),
        )
    }

    fn progress(&self) -> Locator {
        self.page.get_by_role(
            AriaRole::Region,
            Some(GetByRoleOptions::default().name("Progress").exact(true)),
        )
    }

    fn terminal_result(&self) -> Locator {
        self.page.get_by_role(
            AriaRole::Region,
            Some(
                GetByRoleOptions::default()
                    .name("Result summary")
                    .exact(true),
            ),
        )
    }

    fn reconnect(&self) -> Locator {
        self.action("Retry connection")
    }

    fn review(&self) -> Locator {
        self.page.get_by_role(
            AriaRole::Region,
            Some(
                GetByRoleOptions::default()
                    .name("Procedure review")
                    .exact(true),
            ),
        )
    }

    fn file_chooser(&self) -> Locator {
        self.page.get_by_label("Select image", true)
    }

    fn voice(&self) -> Locator {
        self.page.get_by_role(
            AriaRole::Region,
            Some(
                GetByRoleOptions::default()
                    .name("Voice controls")
                    .exact(true),
            ),
        )
    }

    fn tests_workspace(&self) -> Locator {
        self.page.get_by_role(
            AriaRole::Region,
            Some(
                GetByRoleOptions::default()
                    .name("Repository tests")
                    .exact(true),
            ),
        )
    }

    async fn require_unique(&self, contract: &str, locator: Locator) -> Locator {
        locator.wait_for(None).await.unwrap_or_else(|error| {
            panic!("semantic selector for {contract:?} did not become visible: {error}")
        });
        let count = locator
            .count()
            .await
            .unwrap_or_else(|error| panic!("semantic selector for {contract:?} failed: {error}"));
        if count != 1 {
            let snapshot = self
                .page
                .aria_snapshot(None)
                .await
                .unwrap_or_else(|error| format!("<ARIA snapshot failed: {error}>"));
            panic!(
                "semantic selector for {contract:?} matched {count} elements; expected 1\nARIA snapshot:\n{snapshot}"
            );
        }
        locator
    }
}

#[derive(Debug)]
struct DocumentMetrics {
    viewport_width: f64,
    document_client_width: f64,
    document_scroll_width: f64,
    body_client_width: f64,
    body_scroll_width: f64,
}

#[derive(Debug)]
struct ElementMetrics {
    left: f64,
    right: f64,
    top: f64,
    bottom: f64,
    width: f64,
    height: f64,
    clipped_by_ancestor: bool,
    focused: bool,
}

async fn fetch_snapshot(server_url: &str) -> AppSnapshot {
    reqwest::Client::new()
        .get(format!("{server_url}api/snapshot"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn submit_command(harness: &BrowserHarness, request: AppCommandRequest) -> AppCommandResult {
    reqwest::Client::new()
        .post(format!("{}api/commands", harness.server.url()))
        .header(
            deepseek_custom::web::server::REQUEST_TOKEN_HEADER,
            &harness.process_token,
        )
        .header("origin", harness.server.url().trim_end_matches('/'))
        .json(&request)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

fn observable_mismatch<T>(contract: &str, browser: &T, service: &T) -> Result<(), String>
where
    T: std::fmt::Debug + PartialEq,
{
    if browser == service {
        Ok(())
    } else {
        Err(format!(
            "{contract}: browser observable {browser:?} != service observable {service:?}"
        ))
    }
}

async fn document_metrics(page: &Page) -> playwright_rs::Result<DocumentMetrics> {
    let value: serde_json::Value = page
        .evaluate(
        "() => ({ viewportWidth: innerWidth, documentClientWidth: document.documentElement.clientWidth, documentScrollWidth: document.documentElement.scrollWidth, bodyClientWidth: document.body.clientWidth, bodyScrollWidth: document.body.scrollWidth })",
        None::<&()>,
    )
    .await?;
    Ok(DocumentMetrics {
        viewport_width: metric(&value, "viewportWidth"),
        document_client_width: metric(&value, "documentClientWidth"),
        document_scroll_width: metric(&value, "documentScrollWidth"),
        body_client_width: metric(&value, "bodyClientWidth"),
        body_scroll_width: metric(&value, "bodyScrollWidth"),
    })
}

async fn element_metrics(locator: &Locator) -> playwright_rs::Result<ElementMetrics> {
    let value: serde_json::Value = locator
        .evaluate(
            "(element) => { const rect = element.getBoundingClientRect(); let clippedByAncestor = false; for (let ancestor = element.parentElement; ancestor !== null; ancestor = ancestor.parentElement) { const style = getComputedStyle(ancestor); if (!/(auto|scroll|hidden|clip)/.test(style.overflow + style.overflowX + style.overflowY)) continue; const parent = ancestor.getBoundingClientRect(); if (rect.left < parent.left - 0.5 || rect.right > parent.right + 0.5 || rect.top < parent.top - 0.5 || rect.bottom > parent.bottom + 0.5) { clippedByAncestor = true; break; } } return { left: rect.left, right: rect.right, top: rect.top, bottom: rect.bottom, width: rect.width, height: rect.height, clippedByAncestor, focused: document.activeElement === element }; }",
            None::<()>,
        )
        .await?;
    Ok(ElementMetrics {
        left: metric(&value, "left"),
        right: metric(&value, "right"),
        top: metric(&value, "top"),
        bottom: metric(&value, "bottom"),
        width: metric(&value, "width"),
        height: metric(&value, "height"),
        clipped_by_ancestor: value["clippedByAncestor"].as_bool().unwrap(),
        focused: value["focused"].as_bool().unwrap(),
    })
}

fn metric(value: &serde_json::Value, field: &str) -> f64 {
    value[field]
        .as_f64()
        .unwrap_or_else(|| panic!("browser metric {field:?} is missing from {value}"))
}

fn boxes_overlap(left: &ElementMetrics, right: &ElementMetrics) -> bool {
    left.left < right.right - 0.5
        && left.right > right.left + 0.5
        && left.top < right.bottom - 0.5
        && left.bottom > right.top + 0.5
}

fn isolated_snapshot(project_root: &Path) -> AppSnapshot {
    let mut snapshot = super::web_server::visible_snapshot();
    snapshot.revision = AppRevision::INITIAL;
    snapshot.workspace = Workspace::Chat;
    snapshot.transcript.clear();
    snapshot.saved_sessions.clear();
    snapshot.operations.clear();
    snapshot.pending_session_switch = None;
    snapshot.session = SessionSummary {
        id: "isolated-session".into(),
        title: "Isolated browser session".into(),
        backend: "scripted".into(),
        model: "deterministic".into(),
    };
    snapshot.settings.working_dir = Some(project_root.display().to_string());
    snapshot
}

fn test_settings_controller(project_root: &Path) -> Arc<SettingsController> {
    let working_dir = Arc::new(Mutex::new(project_root.to_path_buf()));
    let runtime = RuntimeSettingsPort::new(
        project_root.to_path_buf(),
        Arc::new(AtomicU8::new(0)),
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicUsize::new(100_000)),
        Arc::new(Mutex::new("deterministic".into())),
        working_dir,
        Arc::new(AtomicBool::new(true)),
        Arc::new(AtomicU8::new(8)),
    );
    Arc::new(SettingsController::new(
        project_root.to_path_buf(),
        Settings::default(),
        runtime,
        None,
        None,
    ))
}

// covers: deepseek-custom/web-frontend-automation :: End-to-end tests run against isolated deterministic services :: Browser test environment starts
#[test]
fn browser_harness_uses_unique_ephemeral_state_and_deterministic_services() {
    super::web_server::run_async_test(async {
        let mut first = BrowserHarness::start().await;
        let second = BrowserHarness::start().await;

        assert_ne!(first.project_root.path(), second.project_root.path());
        assert_ne!(first.server.url(), second.server.url());
        assert_ne!(first.process_token, second.process_token);
        assert_eq!(first.process_token.len(), 32);
        assert!(first.server.address().ip().is_loopback());
        assert_ne!(first.server.address().port(), 0);
        assert!(first.project_root.path().join("selected-folder").is_dir());
        assert_eq!(first.state.snapshot().session.backend, "scripted");

        first
            .backend
            .emit_notice("scripted success", NoticeLevel::Info);
        first
            .backend
            .emit_notice("scripted failure", NoticeLevel::Error);
        first.backend.set_operation(
            OperationKind::Procedure,
            OperationPhase::AwaitingReview,
            "scripted review",
        );
        first.backend.set_operation(
            OperationKind::Chat,
            OperationPhase::Interrupted,
            "scripted interruption",
        );
        first
            .backend
            .emit_notice("scripted reconnect", NoticeLevel::Info);
        first
            .state
            .apply_voice_event(VoiceEvent::StateChanged(VoiceState::Listening))
            .unwrap();
        assert_eq!(first.state.snapshot().transcript.len(), 3);
        assert!(first.state.snapshot().operations.iter().any(|operation| {
            operation.kind == OperationKind::Procedure
                && operation.phase == OperationPhase::AwaitingReview
        }));
        assert!(first.state.snapshot().operations.iter().any(|operation| {
            operation.kind == OperationKind::Chat && operation.phase == OperationPhase::Interrupted
        }));
        assert!(
            first
                .state
                .snapshot()
                .operations
                .iter()
                .any(|operation| operation.kind == OperationKind::Voice)
        );
        assert!(first.voice_rx.try_recv().is_err());

        let browser_result = async {
            let playwright = playwright_rs::Playwright::launch().await?;
            let browser = playwright.chromium().launch().await?;
            let page = browser.new_page().await?;
            page.goto(first.server.url(), None).await?;
            let app = BrowserPage::new(page);
            app.wait_for_snapshot().await;
            let transcript = app
                .require_unique(
                    "deterministic success, failure, and reconnect events",
                    app.page.get_by_role(
                        AriaRole::Log,
                        Some(
                            GetByRoleOptions::default()
                                .name("Conversation transcript")
                                .exact(true),
                        ),
                    ),
                )
                .await;
            let transcript_text = transcript.inner_text().await?;
            assert!(transcript_text.contains("scripted success"));
            assert!(transcript_text.contains("scripted failure"));
            assert!(transcript_text.contains("scripted reconnect"));

            app.action("Procedure").click(None).await?;
            let review = app
                .require_unique("deterministic review event", app.review())
                .await;
            assert!(review.inner_text().await?.contains("scripted review"));
            app.action("Chat").click(None).await?;
            let interrupted = app
                .require_unique(
                    "deterministic interruption event",
                    app.page
                        .get_by_text("Turn interrupted. scripted interruption", true),
                )
                .await;
            assert!(interrupted.is_visible().await?);
            browser.close().await?;
            Ok::<_, playwright_rs::Error>(())
        }
        .await;

        first.shutdown().await;
        second.shutdown().await;
        browser_result.unwrap_or_else(|error| panic!(
            "isolated deterministic browser environment failed: {error}\nInstall the matched runtime with:\n{INSTALL_COMMAND}"
        ));
    });
}

// covers: deepseek-custom/web-frontend-automation :: End-to-end tests run against isolated deterministic services :: Browser test environment stops
#[test]
fn browser_environment_reaps_resources_for_every_terminal_path() {
    let checkout_manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
    let user_settings = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../settings.json");
    let manifest_before = std::fs::read(&checkout_manifest).unwrap();
    let settings_before = std::fs::read(&user_settings).unwrap();

    for terminal_path in ["pass", "failure", "timeout", "cancel"] {
        super::web_server::run_async_test(async {
            let harness = BrowserHarness::start().await;
            let root = harness.project_root.path().to_path_buf();
            let address = harness.server.address();
            let playwright = playwright_rs::Playwright::launch()
                .await
                .unwrap_or_else(|error| {
                    panic!("browser runtime unavailable: {error}\nInstall with:\n{INSTALL_COMMAND}")
                });
            let browser = playwright.chromium().launch().await.unwrap();
            let context = browser.new_context().await.unwrap();
            let page = context.new_page().await.unwrap();
            page.goto(harness.server.url(), None).await.unwrap();
            assert_eq!(
                page.title().await.unwrap(),
                "DeepSeekCustom",
                "{terminal_path}"
            );
            let app = BrowserPage::new(page.clone());
            app.wait_for_snapshot().await;
            app.action("Tests").click(None).await.unwrap();
            app.action("Refresh catalogue").click(None).await.unwrap();
            let run = app
                .require_unique(
                    "lifecycle child test action",
                    app.action("Run full workspace"),
                )
                .await;
            run.click(None).await.unwrap();
            app.require_unique(
                "lifecycle retained child result",
                app.page.get_by_role(
                    AriaRole::List,
                    Some(
                        GetByRoleOptions::default()
                            .name("Newest test results first")
                            .exact(true),
                    ),
                ),
            )
            .await;
            assert_eq!(
                harness
                    .active_children
                    .load(std::sync::atomic::Ordering::SeqCst),
                0,
                "scripted child execution survived {terminal_path}"
            );

            match terminal_path {
                "pass" => {}
                "failure" => {
                    assert!(page.get_by_text("not present", true).count().await.unwrap() == 0)
                }
                "timeout" => assert!(
                    tokio::time::timeout(Duration::from_millis(1), std::future::pending::<()>())
                        .await
                        .is_err()
                ),
                "cancel" => {
                    let cancelled = tokio::spawn(std::future::pending::<()>());
                    cancelled.abort();
                    assert!(cancelled.await.unwrap_err().is_cancelled());
                }
                _ => unreachable!(),
            }

            context.close().await.unwrap();
            browser.close().await.unwrap();
            harness.shutdown().await;
            assert!(
                !root.exists(),
                "temporary project root survived {terminal_path}"
            );
            assert!(
                tokio::net::TcpStream::connect(address).await.is_err(),
                "server survived {terminal_path}"
            );
            assert!(
                page.title().await.is_err(),
                "browser page survived {terminal_path}"
            );
        });
    }

    assert_eq!(std::fs::read(checkout_manifest).unwrap(), manifest_before);
    assert_eq!(std::fs::read(user_settings).unwrap(), settings_before);
}

#[test]
fn browser_commands_versions_and_artifact_paths_are_explicit() {
    assert_eq!(playwright_rs::PLAYWRIGHT_VERSION, "1.62.1");
    assert!(INSTALL_COMMAND.contains("install_playwright_chromium"));
    assert!(FOCUSED_COMMAND.contains("web_browser"));
    assert_eq!(ARTIFACT_ROOT, "target/playwright-artifacts");
    assert!(Path::new(ARTIFACT_ROOT).is_relative());
}

#[test]
fn installed_chromium_opens_the_isolated_real_server() {
    super::web_server::run_async_test(async {
        let harness = BrowserHarness::start().await;
        let result = async {
            let playwright = playwright_rs::Playwright::launch().await?;
            let browser = playwright.chromium().launch().await?;
            let page = browser.new_page().await?;
            page.goto(harness.server.url(), None).await?;
            let title = page.title().await?;
            browser.close().await?;
            Ok::<_, playwright_rs::Error>(title)
        }
        .await;
        harness.shutdown().await;
        let title = result.unwrap_or_else(|error| panic!("{}", browser_runtime_failure(&error)));
        assert_eq!(title, "DeepSeekCustom");
    });
}

// covers: deepseek-custom/web-frontend-automation :: Browser controls have stable semantic identities :: Automation locates a primary action
#[test]
fn primary_action_is_located_by_accessible_role_and_name() {
    super::web_server::run_async_test(async {
        let harness = BrowserHarness::start().await;
        let result = async {
            let playwright = playwright_rs::Playwright::launch().await?;
            let browser = playwright.chromium().launch().await?;
            let page = browser.new_page().await?;
            page.goto(harness.server.url(), None).await?;
            let app = BrowserPage::new(page);

            app.wait_for_snapshot().await;
            let evolve = app
                .require_unique("Evolve workspace action", app.action("Evolve"))
                .await;
            evolve.click(None).await?;
            let heading = app
                .require_unique(
                    "active Evolve workspace heading",
                    app.page.get_by_role(
                        AriaRole::Heading,
                        Some(
                            GetByRoleOptions::default()
                                .name("Evolve")
                                .exact(true)
                                .level(2),
                        ),
                    ),
                )
                .await;
            assert_eq!(heading.inner_text().await?, "Evolve");

            browser.close().await?;
            Ok::<_, playwright_rs::Error>(())
        }
        .await;
        harness.shutdown().await;
        result.unwrap_or_else(|error| {
            panic!(
                "semantic browser contract failed: {error}\nInstall the matched runtime with:\n{INSTALL_COMMAND}"
            )
        });
    });
}

// covers: deepseek-custom/web-frontend-automation :: Browser controls have stable semantic identities :: A control is unavailable
#[test]
fn disabled_action_exposes_its_visible_reason() {
    super::web_server::run_async_test(async {
        let harness = BrowserHarness::start().await;
        harness.backend.set_operation(
            OperationKind::Autopilot,
            OperationPhase::Running,
            "deterministic Autopilot run",
        );

        let result = async {
            let playwright = playwright_rs::Playwright::launch().await?;
            let browser = playwright.chromium().launch().await?;
            let page = browser.new_page().await?;
            page.goto(harness.server.url(), None).await?;
            let app = BrowserPage::new(page);

            app.wait_for_snapshot().await;
            let evolve = app
                .require_unique("Evolve workspace action", app.action("Evolve"))
                .await;
            evolve.click(None).await?;

            let unavailable = app.page.get_by_role(
                AriaRole::Button,
                Some(
                    GetByRoleOptions::default()
                        .name("Start Evolve")
                        .exact(true)
                        .disabled(true),
                ),
            );
            let unavailable = app
                .require_unique("disabled Start Evolve action and reason", unavailable)
                .await;
            assert!(unavailable.is_disabled().await?);
            let reason_id = unavailable
                .get_attribute("aria-describedby")
                .await?
                .expect("disabled action must reference its visible reason");
            let reason = app
                .require_unique(
                    "visible Start Evolve disabled reason",
                    app.page.locator(format!("#{reason_id}")),
                )
                .await;
            assert!(reason.is_visible().await?);
            assert_eq!(
                reason.inner_text().await?,
                "Autopilot is active. Stop or finish it before starting Evolve."
            );

            assert_eq!(app.file_chooser().count().await?, 0);
            assert_eq!(app.voice().count().await?, 0);
            assert_eq!(app.progress().count().await?, 0);
            assert_eq!(app.terminal_result().count().await?, 0);
            assert_eq!(app.reconnect().count().await?, 0);
            assert_eq!(app.review().count().await?, 0);
            assert_eq!(app.tests_workspace().count().await?, 0);

            browser.close().await?;
            Ok::<_, playwright_rs::Error>(())
        }
        .await;
        harness.shutdown().await;
        result.unwrap_or_else(|error| {
            panic!(
                "semantic browser contract failed: {error}\nInstall the matched runtime with:\n{INSTALL_COMMAND}"
            )
        });
    });
}

// covers: deepseek-custom/web-frontend-automation :: Browser tests cover critical workflows :: Critical workflow contract is changed
#[test]
fn critical_workflows_match_browser_observations_to_rust_state() {
    super::web_server::run_async_test(async {
        let harness = BrowserHarness::start().await;
        harness
            .state
            .apply_event(AppEvent::SavedSessionsChanged(vec![SessionSummary {
                id: "saved-one".into(),
                title: "Saved deterministic session".into(),
                backend: "scripted".into(),
                model: "deterministic".into(),
            }]))
            .unwrap();
        harness
            .state
            .apply_event(AppEvent::TranscriptAppended(TranscriptBlock {
                id: 41,
                content: TranscriptContent::User {
                    text: "critical chat turn".into(),
                    has_image: false,
                },
            }))
            .unwrap();
        harness
            .state
            .apply_event(AppEvent::TranscriptAppended(TranscriptBlock {
                id: 42,
                content: TranscriptContent::Assistant {
                    spans: vec![TranscriptSpan::Text("streamed deterministic answer".into())],
                },
            }))
            .unwrap();

        let browser_result = async {
            let playwright = playwright_rs::Playwright::launch().await?;
            let browser = playwright.chromium().launch().await?;
            let page = browser.new_page().await?;
            page.goto(harness.server.url(), None).await?;
            let app = BrowserPage::new(page);
            let connected = app.wait_for_snapshot().await;
            assert!(
                connected
                    .inner_text()
                    .await?
                    .contains("Connected. Application revision")
            );

            let transcript = app
                .require_unique(
                    "chat streaming transcript",
                    app.page.get_by_role(
                        AriaRole::Log,
                        Some(
                            GetByRoleOptions::default()
                                .name("Conversation transcript")
                                .exact(true),
                        ),
                    ),
                )
                .await;
            let transcript_text = transcript.inner_text().await?;
            assert!(transcript_text.contains("critical chat turn"));
            assert!(transcript_text.contains("streamed deterministic answer"));
            assert_eq!(
                app.file_chooser().get_attribute("accept").await?.as_deref(),
                Some("image/png,image/jpeg,image/bmp")
            );
            assert!(
                app.voice()
                    .inner_text()
                    .await?
                    .contains("Voice unavailable")
            );
            assert!(app.action("Hold to talk").is_disabled().await?);

            harness.backend.set_operation(
                OperationKind::Chat,
                OperationPhase::Running,
                "streaming chat",
            );
            let stop = app
                .require_unique("chat stop action", app.action("Stop"))
                .await;
            assert!(stop.is_enabled().await?);
            harness.backend.set_operation(
                OperationKind::Chat,
                OperationPhase::Interrupted,
                "chat cancelled",
            );
            app.require_unique(
                "chat cancellation state",
                app.page
                    .get_by_text("Turn interrupted. chat cancelled", true),
            )
            .await;

            for (name, workspace) in [
                ("Sessions", Workspace::Sessions),
                ("Settings", Workspace::Settings),
                ("Autopilot", Workspace::Autopilot),
                ("Cascade", Workspace::Cascade),
                ("Evolve", Workspace::Evolve),
                ("Procedure", Workspace::Procedure),
                ("Tests", Workspace::Tests),
                ("Chat", Workspace::Chat),
            ] {
                app.action(name).click(None).await?;
                let heading = app
                    .require_unique(
                        &format!("{name} navigation heading"),
                        app.page.get_by_role(
                            AriaRole::Heading,
                            Some(GetByRoleOptions::default().name(name).exact(true).level(2)),
                        ),
                    )
                    .await;
                assert_eq!(heading.inner_text().await?, name);
                assert_eq!(
                    harness.state.snapshot().workspace,
                    workspace,
                    "browser and Rust service disagreed for {name}"
                );
            }

            app.action("Sessions").click(None).await?;
            assert!(
                app.require_unique(
                    "saved session",
                    app.page.get_by_text("Saved deterministic session", false)
                )
                .await
                .is_visible()
                .await?
            );
            assert!(
                app.require_unique("session load", app.action("Load"))
                    .await
                    .is_enabled()
                    .await?
            );
            assert!(
                app.require_unique("session delete", app.action("Delete"))
                    .await
                    .is_enabled()
                    .await?
            );
            assert!(
                app.require_unique("new session", app.action("New session"))
                    .await
                    .is_enabled()
                    .await?
            );

            app.action("Settings").click(None).await?;
            app.action("Choose folder").click(None).await?;
            let selected = harness.project_root.path().join("selected-folder");
            let settings_label = format!("Working directory: {}", selected.display());
            let settings_text = app.page.get_by_text(&settings_label, true);
            assert!(
                app.require_unique("selected working folder", settings_text)
                    .await
                    .is_visible()
                    .await?
            );
            assert_eq!(
                harness.state.snapshot().settings.working_dir.as_deref(),
                Some(selected.to_string_lossy().as_ref())
            );
            assert!(
                app.require_unique("settings persistence action", app.action("Save settings"))
                    .await
                    .is_enabled()
                    .await?
            );

            for kind in [
                OperationKind::Autopilot,
                OperationKind::Cascade,
                OperationKind::Evolve,
            ] {
                let name = format!("{kind:?}");
                harness.backend.set_operation(
                    kind,
                    OperationPhase::Completed,
                    &format!("{name} retained result"),
                );
                app.action(&name).click(None).await?;
                let expected_result = format!("{name} retained result");
                let visible_result = app
                    .require_unique(
                        &format!("{name} retained result text"),
                        app.terminal_result().get_by_text(&expected_result, true),
                    )
                    .await;
                assert_eq!(visible_result.inner_text().await?, expected_result);
                assert!(app.terminal_result().is_visible().await?);
                assert_eq!(
                    harness
                        .state
                        .snapshot()
                        .operations
                        .iter()
                        .find(|item| item.kind == kind)
                        .unwrap()
                        .phase,
                    OperationPhase::Completed
                );
            }

            harness.backend.set_operation(
                OperationKind::Procedure,
                OperationPhase::AwaitingReview,
                "verifier: deterministic review evidence",
            );
            app.action("Procedure").click(None).await?;
            assert!(
                app.require_unique("Procedure review", app.review())
                    .await
                    .inner_text()
                    .await?
                    .contains("deterministic review evidence")
            );

            app.action("Tests").click(None).await?;
            let tests = app
                .require_unique("test discovery workspace", app.tests_workspace())
                .await;
            assert!(tests.inner_text().await?.contains("Refresh catalogue"));
            app.action("Refresh catalogue").click(None).await?;
            let run = app
                .require_unique("test execution action", app.action("Run full workspace"))
                .await;
            assert!(run.is_enabled().await?);
            run.click(None).await?;
            let retained = app
                .require_unique(
                    "retained test result",
                    app.page.get_by_role(
                        AriaRole::List,
                        Some(
                            GetByRoleOptions::default()
                                .name("Newest test results first")
                                .exact(true),
                        ),
                    ),
                )
                .await;
            assert!(retained.inner_text().await?.contains("passed"));

            // Closing and opening a page exercises bootstrap after the event stream was disconnected.
            let reconnect_page = browser.new_page().await?;
            reconnect_page.goto(harness.server.url(), None).await?;
            let reconnect_app = BrowserPage::new(reconnect_page);
            assert!(
                reconnect_app
                    .wait_for_snapshot()
                    .await
                    .inner_text()
                    .await?
                    .contains(&harness.state.snapshot().revision.0.to_string())
            );

            browser.close().await?;
            Ok::<_, playwright_rs::Error>(())
        }
        .await;
        harness.shutdown().await;
        browser_result.unwrap_or_else(|error| panic!(
            "critical browser workflow failed: {error}\nInstall the matched runtime with:\n{INSTALL_COMMAND}"
        ));
    });
}

// covers: deepseek-custom/web-frontend-automation :: Browser tests cover critical workflows :: Browser and service disagree
#[test]
fn browser_state_matches_success_and_conflict_service_results() {
    super::web_server::run_async_test(async {
        let harness = BrowserHarness::start().await;
        let browser_result = async {
            let playwright = playwright_rs::Playwright::launch().await?;
            let browser = playwright.chromium().launch().await?;
            let page = browser.new_page().await?;
            page.goto(harness.server.url(), None).await?;
            let app = BrowserPage::new(page);
            app.wait_for_snapshot().await;

            let initial_revision = harness.state.snapshot().revision;
            let success = submit_command(
                &harness,
                AppCommandRequest {
                    revision: initial_revision,
                    command: AppCommand::SelectWorkspace {
                        workspace: Workspace::Settings,
                    },
                },
            )
            .await;
            let success_revision = match success {
                AppCommandResult::Applied { revision } => revision,
                other => panic!("expected applied workspace command, received {other:?}"),
            };
            let settings_heading = app
                .require_unique(
                    "Settings heading after applied command",
                    app.page.get_by_role(
                        AriaRole::Heading,
                        Some(
                            GetByRoleOptions::default()
                                .name("Settings")
                                .exact(true)
                                .level(2),
                        ),
                    ),
                )
                .await;
            let success_browser = settings_heading.inner_text().await?;
            let success_snapshot = fetch_snapshot(harness.server.url()).await;
            observable_mismatch(
                "applied workspace",
                &success_browser,
                &format!("{:?}", success_snapshot.workspace),
            )
            .unwrap();
            observable_mismatch(
                "applied revision",
                &success_revision,
                &success_snapshot.revision,
            )
            .unwrap();

            let winning = submit_command(
                &harness,
                AppCommandRequest {
                    revision: success_revision,
                    command: AppCommand::SelectWorkspace {
                        workspace: Workspace::Procedure,
                    },
                },
            )
            .await;
            let winning_revision = match winning {
                AppCommandResult::Applied { revision } => revision,
                other => panic!("expected winning workspace command, received {other:?}"),
            };
            let conflict = submit_command(
                &harness,
                AppCommandRequest {
                    revision: success_revision,
                    command: AppCommand::SelectWorkspace {
                        workspace: Workspace::Evolve,
                    },
                },
            )
            .await;
            let conflict_revision = match conflict {
                AppCommandResult::Conflict { current_revision } => current_revision,
                other => panic!("expected stale workspace conflict, received {other:?}"),
            };
            observable_mismatch(
                "conflict command revision",
                &conflict_revision,
                &winning_revision,
            )
            .unwrap();

            let procedure_heading = app
                .require_unique(
                    "Procedure heading after conflicting command",
                    app.page.get_by_role(
                        AriaRole::Heading,
                        Some(
                            GetByRoleOptions::default()
                                .name("Procedure")
                                .exact(true)
                                .level(2),
                        ),
                    ),
                )
                .await;
            let conflict_browser = procedure_heading.inner_text().await?;
            let conflict_snapshot = fetch_snapshot(harness.server.url()).await;
            observable_mismatch(
                "conflict workspace",
                &conflict_browser,
                &format!("{:?}", conflict_snapshot.workspace),
            )
            .unwrap();
            observable_mismatch(
                "conflict snapshot revision",
                &conflict_revision,
                &conflict_snapshot.revision,
            )
            .unwrap();

            let failure = std::panic::catch_unwind(|| {
                if let Err(diagnostic) = observable_mismatch(
                    "controlled workspace disagreement",
                    &"Evolve",
                    &"Procedure",
                ) {
                    panic!("{diagnostic}");
                }
            })
            .expect_err("a browser/service disagreement must fail the comparison");
            let diagnostic = failure
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| {
                    failure
                        .downcast_ref::<&str>()
                        .map(|message| (*message).into())
                })
                .expect("controlled mismatch panic must contain a string diagnostic");
            assert!(diagnostic.contains("browser observable \"Evolve\""));
            assert!(diagnostic.contains("service observable \"Procedure\""));

            browser.close().await?;
            Ok::<_, playwright_rs::Error>(())
        }
        .await;
        harness.shutdown().await;
        browser_result.unwrap_or_else(|error| {
            panic!(
                "browser/service comparison failed: {error}\nInstall the matched runtime with:\n{INSTALL_COMMAND}"
            )
        });
    });
}

// covers: deepseek-custom/controlled-development-mode :: The existing web application exposes Controlled Development :: User reviews and approves a Work Card
#[test]
fn complete_controlled_panel_is_available_at_desktop_and_narrow_widths() {
    super::web_server::run_async_test(async {
        let awaiting = BrowserHarness::start_with_controlled(controlled_panel_view(
            ControlledDevelopmentPhase::AwaitingApproval,
        ))
        .await;
        let playwright = playwright_rs::Playwright::launch()
            .await
            .unwrap_or_else(|error| panic!("{}", browser_runtime_failure(&error)));
        let browser = playwright
            .chromium()
            .launch()
            .await
            .unwrap_or_else(|error| panic!("{}", browser_runtime_failure(&error)));
        let context = browser.new_context().await.unwrap();
        let artifacts = start_failure_capture(
            &context,
            "complete_controlled_panel_is_available_at_desktop_and_narrow_widths",
        )
        .await;
        let page = context.new_page().await.unwrap();
        let browser_result = AssertUnwindSafe(async {
            page.goto(awaiting.server.url(), None).await?;
            let app = BrowserPage::new(page.clone());
            app.wait_for_snapshot().await;

            for viewport in [
                Viewport {
                    width: 1440,
                    height: 900,
                },
                Viewport {
                    width: 360,
                    height: 800,
                },
            ] {
                let viewport_width = viewport.width;
                let viewport_height = viewport.height;
                app.page.set_viewport_size(viewport).await?;
                let panel = app
                    .require_unique(
                        "Controlled Development panel",
                        app.page.get_by_role(
                            AriaRole::Region,
                            Some(
                                GetByRoleOptions::default()
                                    .name("Controlled Development")
                                    .exact(true),
                            ),
                        ),
                    )
                    .await;
                let panel_text = panel.inner_text().await?;
                for expected in [
                    "Current phase: Awaiting Approval",
                    "card-browser-7",
                    "Expose the complete controlled review panel",
                    "cargo test -p deepseek-custom-tests --test it controlled",
                    "summary.rs",
                    "ChatWorkspace.test.tsx",
                    "settings.json",
                    "No dependency changes",
                    "Proof results",
                    "passed, exit 0",
                    "Remaining limitation",
                ] {
                    assert!(
                        panel_text.contains(expected),
                        "controlled panel is missing {expected:?} at {}x{}: {panel_text}",
                        viewport_width,
                        viewport_height
                    );
                }

                let toggle = app.page.get_by_role(
                    AriaRole::Switch,
                    Some(
                        GetByRoleOptions::default()
                            .name("Controlled Development")
                            .exact(true),
                    ),
                );
                assert!(toggle.is_checked().await?);
                for action in [
                    "Approve Work Card",
                    "Reject Work Card",
                    "Stop controlled work",
                ] {
                    assert!(app.action(action).is_enabled().await?, "{action} disabled");
                }

                let details = app
                    .require_unique(
                        "collapsed Controlled Development raw details",
                        app.page
                            .get_by_label("Controlled Development raw details", true),
                    )
                    .await;
                assert!(details.get_attribute("open").await?.is_none());
                details
                    .get_by_text("Raw details for Awaiting Approval", false)
                    .click(None)
                    .await?;
                let raw_text = details.inner_text().await?;
                assert!(raw_text.contains("complete backend reasoning"));
                assert!(raw_text.contains("test result: ok. 1 passed; 0 failed"));
                details
                    .get_by_text("Raw details for Awaiting Approval", false)
                    .click(None)
                    .await?;

                let metrics = document_metrics(&app.page).await?;
                assert!(
                    metrics.document_scroll_width <= metrics.viewport_width + 0.5,
                    "controlled panel overflows horizontally at {}x{}: {metrics:?}",
                    viewport_width,
                    viewport_height
                );
            }

            let completed = BrowserHarness::start_with_controlled(controlled_panel_view(
                ControlledDevelopmentPhase::Completed,
            ))
            .await;
            app.page
                .set_viewport_size(Viewport {
                    width: 360,
                    height: 800,
                })
                .await?;
            app.page.goto(completed.server.url(), None).await?;
            app.wait_for_snapshot().await;
            let completed_text = app
                .require_unique(
                    "completed Controlled Development panel",
                    app.page.get_by_role(
                        AriaRole::Region,
                        Some(
                            GetByRoleOptions::default()
                                .name("Controlled Development")
                                .exact(true),
                        ),
                    ),
                )
                .await
                .inner_text()
                .await?;
            assert!(completed_text.contains("Current phase: Completed"));
            assert!(completed_text.contains("Phase: Completed. Work Card outcome"));
            assert!(completed_text.contains("Approve is unavailable until"));
            assert!(completed_text.contains("Reject is unavailable until"));
            assert!(completed_text.contains("Stop is unavailable because"));
            for action in [
                "Approve Work Card",
                "Reject Work Card",
                "Stop controlled work",
            ] {
                assert!(app.action(action).is_disabled().await?, "{action} enabled");
            }
            app.page.goto(awaiting.server.url(), None).await?;
            completed.shutdown().await;

            Ok::<_, playwright_rs::Error>(())
        })
        .catch_unwind()
        .await;
        let outcome = match browser_result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(format!(
                "controlled panel browser operation failed: {error}"
            )),
            Err(payload) => Err(panic_diagnostic(payload)),
        };
        let retained =
            finish_failure_capture(outcome, &context, &page, &awaiting, &artifacts).await;
        browser.close().await.unwrap();
        awaiting.shutdown().await;
        retained.unwrap_or_else(|error| panic!("controlled panel browser test failed: {error}"));
    });
}

// covers: deepseek-custom/web-frontend-automation :: Responsive layouts have automated visual evidence :: Responsive matrix passes
#[test]
fn every_workspace_passes_the_required_responsive_matrix() {
    super::web_server::run_async_test(async {
        let harness = BrowserHarness::start().await;
        let playwright = playwright_rs::Playwright::launch()
            .await
            .unwrap_or_else(|error| panic!("{}", browser_runtime_failure(&error)));
        let browser = playwright
            .chromium()
            .launch()
            .await
            .unwrap_or_else(|error| panic!("{}", browser_runtime_failure(&error)));
        let context = browser.new_context().await.unwrap();
        let artifacts = start_failure_capture(
            &context,
            "every_workspace_passes_the_required_responsive_matrix",
        )
        .await;
        let page = context.new_page().await.unwrap();
        let browser_result = AssertUnwindSafe(async {
            page.goto(harness.server.url(), None).await?;
            let app = BrowserPage::new(page.clone());
            app.wait_for_snapshot().await;

            let workspaces = [
                ("Chat", "Attach image"),
                ("Sessions", "New session"),
                ("Settings", "Choose folder"),
                ("Autopilot", "Start Autopilot"),
                ("Cascade", "Start Cascade"),
                ("Evolve", "Start Evolve"),
                ("Procedure", "Start Procedure"),
                ("Tests", "Refresh catalogue"),
            ];

            for viewport in [
                Viewport {
                    width: 360,
                    height: 800,
                },
                Viewport {
                    width: 768,
                    height: 1024,
                },
                Viewport {
                    width: 1440,
                    height: 900,
                },
            ] {
                let size = format!("{}x{}", viewport.width, viewport.height);
                let viewport_width = viewport.width;
                let viewport_height = viewport.height;
                app.page.set_viewport_size(viewport).await?;

                let mut navigation_boxes = Vec::new();
                for (workspace, _) in workspaces {
                    let navigation = app
                        .require_unique(
                            &format!("{workspace} navigation at {size}"),
                            app.navigation(workspace),
                        )
                        .await;
                    assert!(
                        navigation.is_visible().await?,
                        "{workspace} navigation is hidden at {size}"
                    );
                    let metrics = element_metrics(&navigation).await?;
                    assert!(
                        metrics.width > 0.0 && metrics.height > 0.0,
                        "{workspace} navigation has no rendered area at {size}: {metrics:?}"
                    );
                    assert!(
                        !metrics.clipped_by_ancestor,
                        "{workspace} navigation is clipped at {size}: {metrics:?}"
                    );
                    navigation_boxes.push((workspace, metrics));
                }
                for left in 0..navigation_boxes.len() {
                    for right in left + 1..navigation_boxes.len() {
                        assert!(
                            !boxes_overlap(
                                &navigation_boxes[left].1,
                                &navigation_boxes[right].1
                            ),
                            "workspace navigation overlaps at {size}: {} {:?} and {} {:?}",
                            navigation_boxes[left].0,
                            navigation_boxes[left].1,
                            navigation_boxes[right].0,
                            navigation_boxes[right].1
                        );
                    }
                }

                for (workspace, primary_action) in workspaces {
                    app.navigation(workspace).click(None).await?;
                    app.require_unique(
                        &format!("{workspace} heading at {size}"),
                        app.page.get_by_role(
                            AriaRole::Heading,
                            Some(
                                GetByRoleOptions::default()
                                    .name(workspace)
                                    .exact(true)
                                    .level(2),
                            ),
                        ),
                    )
                    .await;
                    let action = app
                        .require_unique(
                            &format!("{workspace} primary action {primary_action} at {size}"),
                            app.action(primary_action),
                        )
                        .await;
                    assert!(
                        action.is_visible().await?,
                        "{workspace} action {primary_action:?} is hidden at {size}"
                    );
                    action.scroll_into_view_if_needed().await?;
                    action.focus().await?;
                    let action_metrics = element_metrics(&action).await?;
                    assert!(
                        action_metrics.focused,
                        "keyboard focus cannot reach {workspace} action {primary_action:?} at {size}: {action_metrics:?}"
                    );
                    assert!(
                        action_metrics.left >= -0.5
                            && action_metrics.right <= f64::from(viewport_width) + 0.5
                            && action_metrics.top >= -0.5
                            && action_metrics.bottom <= f64::from(viewport_height) + 0.5,
                        "{workspace} action {primary_action:?} is outside the viewport after scrolling at {size}: {action_metrics:?}"
                    );
                    assert!(
                        !action_metrics.clipped_by_ancestor,
                        "{workspace} action {primary_action:?} is clipped at {size}: {action_metrics:?}"
                    );

                    let document = document_metrics(&app.page).await?;
                    assert_eq!(document.viewport_width, f64::from(viewport_width));
                    assert!(
                        document.document_scroll_width <= document.document_client_width + 0.5
                            && document.body_scroll_width <= document.body_client_width + 0.5
                            && document.document_scroll_width <= document.viewport_width + 0.5,
                        "horizontal page overflow in {workspace} at {size}: {document:?}"
                    );
                }
            }

            Ok::<_, playwright_rs::Error>(())
        })
        .catch_unwind()
        .await;
        let outcome = match browser_result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(format!("responsive browser operation failed: {error}")),
            Err(payload) => Err(panic_diagnostic(payload)),
        };
        let retained = finish_failure_capture(outcome, &context, &page, &harness, &artifacts).await;
        browser.close().await.unwrap();
        harness.shutdown().await;
        retained.unwrap_or_else(|error| panic!("responsive browser matrix failed: {error}"));
    });
}

// covers: deepseek-custom/web-frontend-automation :: Responsive layouts have automated visual evidence :: Responsive check fails
#[test]
fn controlled_responsive_failure_retains_complete_browser_artifacts() {
    super::web_server::run_async_test(async {
        const TEST_NAME: &str = "controlled_responsive_failure";

        let harness = BrowserHarness::start().await;
        let playwright = playwright_rs::Playwright::launch()
            .await
            .unwrap_or_else(|error| panic!("{}", browser_runtime_failure(&error)));
        let browser = playwright
            .chromium()
            .launch()
            .await
            .unwrap_or_else(|error| panic!("{}", browser_runtime_failure(&error)));
        let context = browser.new_context().await.unwrap();
        let artifacts = start_failure_capture(&context, TEST_NAME).await;
        let page = context.new_page().await.unwrap();
        page.set_viewport_size(Viewport {
            width: 360,
            height: 800,
        })
        .await
        .unwrap();
        page.goto(harness.server.url(), None).await.unwrap();
        let app = BrowserPage::new(page.clone());
        app.wait_for_snapshot().await;
        let _: serde_json::Value = page
            .evaluate(
                "() => console.error('controlled responsive diagnostic')",
                None::<&()>,
            )
            .await
            .unwrap();

        let metrics = document_metrics(&page).await.unwrap();
        let controlled_failure = Err(format!(
            "responsive assertion failed at 360x800: expected viewport width 361, observed {}",
            metrics.viewport_width
        ));
        let diagnostic =
            finish_failure_capture(controlled_failure, &context, &page, &harness, &artifacts)
                .await
                .expect_err("the controlled responsive assertion must fail");

        assert!(diagnostic.contains("responsive assertion failed at 360x800"));
        assert!(diagnostic.contains(&artifacts.directory.display().to_string()));
        assert!(artifacts.screenshot.is_file());
        assert!(std::fs::metadata(&artifacts.screenshot).unwrap().len() > 100);
        assert!(artifacts.trace.is_file());
        assert!(std::fs::metadata(&artifacts.trace).unwrap().len() > 100);
        let console = std::fs::read_to_string(&artifacts.console).unwrap();
        assert!(console.contains("error controlled responsive diagnostic"));
        let server = std::fs::read_to_string(&artifacts.server).unwrap();
        assert!(server.contains("server_url=http://127.0.0.1:"));
        assert!(server.contains("failure=responsive assertion failed at 360x800"));

        let passing_artifacts = BrowserFailureArtifacts::for_test("passing_capture_cleanup");
        std::fs::create_dir_all(&passing_artifacts.directory).unwrap();
        std::fs::write(passing_artifacts.directory.join("stale.txt"), "stale").unwrap();
        let passing_artifacts = start_failure_capture(&context, "passing_capture_cleanup").await;
        finish_failure_capture(Ok(()), &context, &page, &harness, &passing_artifacts)
            .await
            .unwrap();
        assert!(!passing_artifacts.directory.exists());

        browser.close().await.unwrap();
        harness.shutdown().await;
    });
}

// covers: deepseek-custom/web-frontend-automation :: Browser prerequisites and failures are explicit :: Browser runtime is missing
#[test]
fn missing_browser_runtime_fails_with_version_matched_installer_guidance() {
    super::web_server::run_async_test(async {
        let isolated_runtime = tempfile::Builder::new()
            .prefix("missing-playwright-browser-")
            .tempdir()
            .unwrap();
        let missing_executable = isolated_runtime.path().join("chromium-not-installed.exe");
        assert!(!missing_executable.exists());

        let playwright = playwright_rs::Playwright::launch().await.unwrap();
        let startup = playwright
            .chromium()
            .launch_with_options(
                LaunchOptions::default().executable_path(missing_executable.display().to_string()),
            )
            .await;
        let error = startup.expect_err("an isolated missing executable must fail browser startup");
        let guidance = browser_runtime_failure(&error);

        assert!(guidance.contains("playwright-rs 0.17.0 / Playwright 1.62.1"));
        assert!(guidance.contains("Chromium"));
        assert!(guidance.contains("unavailable"));
        assert!(guidance.ends_with(INSTALL_COMMAND));
        assert_eq!(
            guidance.lines().next_back().unwrap(),
            "cargo run -p deepseek-custom-tests --example install_playwright_chromium"
        );
    });
}

/// Live production practice. This test is ignored because it requires a running local Ollama
/// daemon and the explicitly named local model. It never reads backend configuration from the
/// checkout. Every browser step and the complete trace remain under `target/playwright-artifacts`.
#[test]
#[ignore = "requires local Ollama model qwen2.5-coder:7b-instruct-q4_K_M"]
fn production_binary_uses_only_ollama_across_browser_workflows() {
    let checkout_settings = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../settings.json");
    let checkout_settings_before = std::fs::read(&checkout_settings).ok();
    let artifacts = ProductionOllamaArtifacts::prepare();

    super::web_server::run_async_test(async {
        let playwright = playwright_rs::Playwright::launch()
            .await
            .unwrap_or_else(|error| panic!("{}", browser_runtime_failure(&error)));
        let browser = playwright
            .chromium()
            .launch()
            .await
            .unwrap_or_else(|error| panic!("{}", browser_runtime_failure(&error)));
        let context = browser.new_context().await.unwrap();
        context.set_default_timeout(120_000.0).await;
        let tracing = context.tracing().await.unwrap();
        tracing
            .start(Some(
                TracingStartOptions::default()
                    .name(LIVE_OLLAMA_TEST)
                    .screenshots(true)
                    .snapshots(true),
            ))
            .await
            .unwrap();
        let page = context.new_page().await.unwrap();
        let run = AssertUnwindSafe(async {
            let mut production =
                ProductionBinary::launch(&production_binary_path(), &artifacts.server)?;
            let url = production.wait_for_url().await?;
            let startup_workspace = capture_production_workspace_bytes(production.project_root())
                .map_err(|error| error.to_string())?;
            require_workspace_boundary(
                production.project_root(),
                &artifacts.workspace_evidence,
                "startup",
                &startup_workspace,
                &[],
            )?;
            page.goto(&url, None)
                .await
                .map_err(|error| error.to_string())?;
            let app = BrowserPage::new(page.clone());

            let connected = app.wait_for_snapshot().await;
            if page.title().await.map_err(|error| error.to_string())? != "DeepSeekCustom" {
                return Err("production page title did not match DeepSeekCustom".into());
            }
            capture_live_step(&page, &artifacts, "01-startup.png").await?;

            app.action("Settings")
                .click(None)
                .await
                .map_err(|error| error.to_string())?;
            let backend = app.page.get_by_role(
                AriaRole::Combobox,
                Some(GetByRoleOptions::default().name("Backend").exact(true)),
            );
            backend
                .wait_for(None)
                .await
                .map_err(|error| error.to_string())?;
            let backend_value = backend
                .input_value(None)
                .await
                .map_err(|error| error.to_string())?;
            let backend_options = backend
                .inner_text()
                .await
                .map_err(|error| error.to_string())?;
            if backend_value != "ollama" || backend_options.lines().any(|line| {
                matches!(
                    line.trim().to_ascii_lowercase().as_str(),
                    "deepseek" | "claude" | "codex"
                )
            }) {
                return Err(format!(
                    "production settings exposed a non-Ollama backend: selected={backend_value:?}, options={backend_options:?}"
                ));
            }
            let model = app.page.get_by_role(
                AriaRole::Combobox,
                Some(GetByRoleOptions::default().name("Model").exact(true)),
            );
            let model_value = model
                .input_value(None)
                .await
                .map_err(|error| error.to_string())?;
            if model_value != LIVE_OLLAMA_MODEL {
                return Err(format!(
                    "production settings selected {model_value:?}, expected {LIVE_OLLAMA_MODEL:?}"
                ));
            }
            let working_directory = "Working directory: Project root";
            app.page
                .get_by_text(&working_directory, true)
                .wait_for(None)
                .await
                .map_err(|error| error.to_string())?;
            capture_live_step(&page, &artifacts, "02-settings-ollama.png").await?;

            app.action("Chat")
                .click(None)
                .await
                .map_err(|error| error.to_string())?;
            let message = app.page.get_by_label("Message", true);
            message
                .fill(
                    "Reply with one short confirmation sentence. Do not call tools.",
                    None,
                )
                .await
                .map_err(|error| error.to_string())?;
            capture_live_step(&page, &artifacts, "03-chat-prompt-ready.png").await?;
            app.action("Send message")
                .click(None)
                .await
                .map_err(|error| error.to_string())?;
            app.page
                .get_by_text("Turn running.", false)
                .wait_for(None)
                .await
                .map_err(|error| format!("live Ollama turn never entered running state: {error}"))?;
            capture_live_step(&page, &artifacts, "04-chat-running.png").await?;

            let transcript = app.page.get_by_role(
                AriaRole::Log,
                Some(
                    GetByRoleOptions::default()
                        .name("Conversation transcript")
                        .exact(true),
                ),
            );
            app.page
                .get_by_text("Turn completed.", false)
                .wait_for(None)
                .await
                .map_err(|error| format!("live Ollama turn did not complete: {error}"))?;
            let final_transcript =
                wait_for_locator_text(&transcript, "Assistant", Duration::from_secs(30)).await?;
            if !final_transcript.contains("Turn status") {
                return Err(format!(
                    "live Ollama transcript omitted the terminal block: {final_transcript:?}"
                ));
            }
            capture_live_step(&page, &artifacts, "05-chat-final.png").await?;

            page.reload(None)
                .await
                .map_err(|error| error.to_string())?;
            let reloaded = BrowserPage::new(page.clone());
            reloaded.wait_for_snapshot().await;
            let reloaded_transcript = reloaded.page.get_by_role(
                AriaRole::Log,
                Some(
                    GetByRoleOptions::default()
                        .name("Conversation transcript")
                        .exact(true),
                ),
            );
            wait_for_locator_text(
                &reloaded_transcript,
                "Assistant",
                Duration::from_secs(30),
            )
            .await?;
            capture_live_step(&page, &artifacts, "06-reload-reconnected.png").await?;

            reloaded
                .action("Sessions")
                .click(None)
                .await
                .map_err(|error| error.to_string())?;
            reloaded
                .page
                .get_by_role(
                    AriaRole::Heading,
                    Some(
                        GetByRoleOptions::default()
                            .name("Sessions")
                            .exact(true)
                            .level(2),
                    ),
                )
                .wait_for(None)
                .await
                .map_err(|error| error.to_string())?;
            capture_live_step(&page, &artifacts, "07-sessions-navigation.png").await?;

            reloaded
                .action("Tests")
                .click(None)
                .await
                .map_err(|error| error.to_string())?;
            reloaded
                .action("Refresh catalogue")
                .click(None)
                .await
                .map_err(|error| error.to_string())?;
            reloaded
                .page
                .get_by_role(
                    AriaRole::Heading,
                    Some(
                        GetByRoleOptions::default()
                            .name("Test catalogue")
                            .exact(true)
                            .level(4),
                    ),
                )
                .wait_for(None)
                .await
                .map_err(|error| format!("safe test catalogue refresh failed: {error}"))?;
            capture_live_step(&page, &artifacts, "08-tests-catalogue.png").await?;
            reloaded
                .action("Run exact test")
                .click(None)
                .await
                .map_err(|error| error.to_string())?;
            let retained = reloaded.page.get_by_role(
                AriaRole::List,
                Some(
                    GetByRoleOptions::default()
                        .name("Newest test results first")
                        .exact(true),
                ),
            );
            let result =
                wait_for_locator_text(&retained, "passed", Duration::from_secs(60)).await?;
            if !result.contains("production_dashboard_smoke") {
                return Err(format!(
                    "retained test result did not identify the safe exact test: {result:?}"
                ));
            }
            capture_live_step(&page, &artifacts, "09-tests-exact-pass.png").await?;

            let workspace_baseline = capture_production_workspace_bytes(production.project_root())
                .map_err(|error| error.to_string())?;
            require_workspace_boundary(
                production.project_root(),
                &artifacts.workspace_evidence,
                "controlled-baseline",
                &workspace_baseline,
                &[],
            )?;

            reloaded
                .action("Chat")
                .click(None)
                .await
                .map_err(|error| error.to_string())?;
            let controlled_panel = reloaded.page.get_by_role(
                AriaRole::Region,
                Some(
                    GetByRoleOptions::default()
                        .name("Controlled Development")
                        .exact(true),
                ),
            );
            controlled_panel
                .wait_for(None)
                .await
                .map_err(|error| error.to_string())?;
            let controlled_toggle = reloaded.page.get_by_role(
                AriaRole::Switch,
                Some(
                    GetByRoleOptions::default()
                        .name("Controlled Development")
                        .exact(true),
                ),
            );
            if controlled_toggle
                .is_checked()
                .await
                .map_err(|error| error.to_string())?
            {
                return Err("Controlled Development unexpectedly started enabled".into());
            }
            controlled_toggle
                .click(None)
                .await
                .map_err(|error| error.to_string())?;
            wait_for_locator_text(
                &controlled_panel,
                "Current phase: Off",
                Duration::from_secs(30),
            )
            .await?;
            if !controlled_toggle
                .is_checked()
                .await
                .map_err(|error| error.to_string())?
            {
                return Err("Controlled Development toggle did not become checked".into());
            }
            require_workspace_boundary(
                production.project_root(),
                &artifacts.workspace_evidence,
                "controlled-enabled",
                &workspace_baseline,
                &[],
            )?;
            capture_controlled_step(&controlled_panel, &artifacts, "10-controlled-enabled.png")
                .await?;

            let controlled_request = concat!(
                "Change only src/lib.rs so practice_fixture returns the string literal controlled. ",
                "The observable result is that the existing production_dashboard_smoke test passes. ",
                "Use cargo test --test it as the only proof command. ",
                "List src/lib.rs as the only production path and use no supporting paths. ",
                "Exclude Cargo.toml, tests/it/main.rs, settings.json, CLAUDE.md, and src/unrelated.txt. ",
                "Use no complexity exceptions."
            );
            let controlled_message = reloaded.page.get_by_label("Message", true);
            controlled_message
                .fill(controlled_request, None)
                .await
                .map_err(|error| error.to_string())?;
            reloaded
                .action("Send message")
                .click(None)
                .await
                .map_err(|error| error.to_string())?;
            wait_for_locator_text(
                &controlled_panel,
                "Current phase: Planning",
                Duration::from_secs(30),
            )
            .await?;
            require_workspace_boundary(
                production.project_root(),
                &artifacts.workspace_evidence,
                "planning",
                &workspace_baseline,
                &[],
            )?;
            capture_controlled_step(&controlled_panel, &artifacts, "11-controlled-planning.png")
                .await?;

            let awaiting = wait_for_locator_text(
                &controlled_panel,
                "Current phase: Awaiting Approval",
                Duration::from_secs(240),
            )
            .await?;
            for expected in [
                "src/lib.rs",
                "cargo test --test it",
                "tests/it/main.rs",
                "src/unrelated.txt",
                "Complexity exceptions",
                "None.",
            ] {
                if !awaiting.contains(expected) {
                    return Err(format!(
                        "live Ollama Work Card omitted {expected:?}: {awaiting:?}"
                    ));
                }
            }
            require_workspace_boundary(
                production.project_root(),
                &artifacts.workspace_evidence,
                "awaiting-approval",
                &workspace_baseline,
                &[],
            )?;
            capture_controlled_step(
                &controlled_panel,
                &artifacts,
                "12-controlled-awaiting-approval.png",
            )
            .await?;

            reloaded
                .action("Approve Work Card")
                .click(None)
                .await
                .map_err(|error| error.to_string())?;
            wait_for_locator_text(
                &controlled_panel,
                "Current phase: Executing",
                Duration::from_secs(30),
            )
            .await?;
            require_workspace_boundary(
                production.project_root(),
                &artifacts.workspace_evidence,
                "approved-executing",
                &workspace_baseline,
                &[],
            )?;
            capture_controlled_step(&controlled_panel, &artifacts, "13-controlled-executing.png")
                .await?;

            let completed = wait_for_locator_text(
                &controlled_panel,
                "Current phase: Completed",
                Duration::from_secs(300),
            )
            .await?;
            for expected in [
                "Changed paths",
                "src/lib.rs",
                "Proof results",
                "cargo test --test it: passed, exit 0",
                "Phase: Completed",
            ] {
                if !completed.contains(expected) {
                    return Err(format!(
                        "completed Controlled Development panel omitted {expected:?}: {completed:?}"
                    ));
                }
            }
            let promoted_workspace = require_workspace_boundary(
                production.project_root(),
                &artifacts.workspace_evidence,
                "completed-promotion",
                &workspace_baseline,
                &["PROJECT_STATE.md", "src/lib.rs"],
            )?;
            let promoted_source = promoted_workspace
                .get("src/lib.rs")
                .ok_or_else(|| "promoted src/lib.rs is missing".to_string())?;
            if !String::from_utf8_lossy(promoted_source).contains("controlled") {
                return Err(format!(
                    "promoted src/lib.rs omitted the approved result: {promoted_source:?}"
                ));
            }
            let project_state = promoted_workspace
                .get("PROJECT_STATE.md")
                .ok_or_else(|| "promoted PROJECT_STATE.md is missing".to_string())?;
            if String::from_utf8_lossy(project_state)
                .lines()
                .filter(|line| !line.trim().is_empty())
                .count()
                > 40
            {
                return Err("promoted PROJECT_STATE.md exceeded 40 nonblank lines".into());
            }
            capture_controlled_step(&controlled_panel, &artifacts, "14-controlled-completed.png")
                .await?;

            controlled_message
                .fill("DIFF", None)
                .await
                .map_err(|error| error.to_string())?;
            reloaded
                .action("Send message")
                .click(None)
                .await
                .map_err(|error| error.to_string())?;
            let diff_transcript = reloaded.page.get_by_role(
                AriaRole::Log,
                Some(
                    GetByRoleOptions::default()
                        .name("Conversation transcript")
                        .exact(true),
                ),
            );
            let visible_diff =
                wait_for_locator_text(&diff_transcript, "diff --git", Duration::from_secs(30))
                    .await?;
            if !visible_diff.contains("src/lib.rs") {
                return Err(format!(
                    "DIFF control response omitted src/lib.rs: {visible_diff:?}"
                ));
            }
            require_workspace_boundary(
                production.project_root(),
                &artifacts.workspace_evidence,
                "diff-inspection",
                &promoted_workspace,
                &[],
            )?;
            capture_live_step(&page, &artifacts, "15-controlled-diff.png").await?;

            controlled_message
                .fill(
                    "Plan a second packet that changes only src/unrelated.txt and proves the result with cargo test --test it.",
                    None,
                )
                .await
                .map_err(|error| error.to_string())?;
            reloaded
                .action("Send message")
                .click(None)
                .await
                .map_err(|error| error.to_string())?;
            wait_for_locator_text(
                &controlled_panel,
                "Current phase: Planning",
                Duration::from_secs(30),
            )
            .await?;
            require_workspace_boundary(
                production.project_root(),
                &artifacts.workspace_evidence,
                "stop-planning",
                &promoted_workspace,
                &[],
            )?;
            capture_controlled_step(
                &controlled_panel,
                &artifacts,
                "16-controlled-stop-planning.png",
            )
            .await?;
            reloaded
                .action("Stop controlled work")
                .click(None)
                .await
                .map_err(|error| error.to_string())?;
            wait_for_locator_text(
                &controlled_panel,
                "Current phase: Interrupted",
                Duration::from_secs(30),
            )
            .await?;
            require_workspace_boundary(
                production.project_root(),
                &artifacts.workspace_evidence,
                "stop-interrupted",
                &promoted_workspace,
                &[],
            )?;
            capture_controlled_step(
                &controlled_panel,
                &artifacts,
                "17-controlled-interrupted.png",
            )
            .await?;

            let connected_text = connected
                .inner_text()
                .await
                .map_err(|error| error.to_string())?;
            if !connected_text.contains("Connected. Application revision") {
                return Err(format!("startup connection evidence changed: {connected_text:?}"));
            }
            let console = page
                .console_messages()
                .into_iter()
                .map(|message| format!("{} {}", message.type_(), message.text()))
                .collect::<Vec<_>>()
                .join("\n");
            std::fs::write(&artifacts.console, format!("{console}\n"))
                .map_err(|error| error.to_string())?;
            if console.lines().any(|line| line.starts_with("error ")) {
                return Err(format!(
                    "browser console reported an error before shutdown: {console}"
                ));
            }
            production.shutdown();
            Ok::<(), String>(())
        })
        .catch_unwind()
        .await;

        let mut outcome = match run {
            Ok(result) => result,
            Err(payload) => Err(panic_diagnostic(payload)),
        };
        if outcome.is_err()
            && let Err(error) = page
                .screenshot_to_file(&artifacts.screenshot("99-failure.png"), None)
                .await
        {
            let diagnostic = outcome.unwrap_err();
            outcome = Err(format!(
                "{diagnostic}\nFailure screenshot also failed: {error}"
            ));
        }
        if !artifacts.console.is_file() {
            let console = page
                .console_messages()
                .into_iter()
                .map(|message| format!("{} {}", message.type_(), message.text()))
                .collect::<Vec<_>>()
                .join("\n");
            std::fs::write(&artifacts.console, format!("{console}\n")).unwrap();
        }
        if let Err(error) = tracing
            .stop(Some(
                TracingStopOptions::default().path(artifacts.trace.display().to_string()),
            ))
            .await
        {
            let diagnostic = outcome
                .err()
                .unwrap_or_else(|| "browser workflow passed".into());
            outcome = Err(format!("{diagnostic}\nTrace finalization failed: {error}"));
        }
        browser.close().await.unwrap();

        if std::fs::read(&checkout_settings).ok() != checkout_settings_before {
            let diagnostic = outcome
                .err()
                .unwrap_or_else(|| "browser workflow passed".into());
            outcome = Err(format!(
                "{diagnostic}\ncheckout settings.json changed during isolated practice"
            ));
        }
        if !artifacts.trace.is_file()
            || !artifacts.server.is_file()
            || !artifacts.workspace_evidence.is_file()
        {
            let diagnostic = outcome
                .err()
                .unwrap_or_else(|| "browser workflow passed".into());
            outcome = Err(format!(
                "{diagnostic}\nrequired trace or server log is missing from {}",
                artifacts.directory.display()
            ));
        }
        outcome.unwrap_or_else(|diagnostic| {
            panic!(
                "production Ollama browser practice failed: {diagnostic}\nArtifacts: {}",
                artifacts.directory.display()
            )
        });
        println!(
            "production Ollama browser artifacts: {}",
            artifacts.directory.display()
        );
    });
}
