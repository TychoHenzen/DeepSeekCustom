//! Isolated Rust-side foundation for browser end-to-end tests.
//!
//! The browser scenarios are added in later tasks. This module owns the
//! deterministic server environment they use and deliberately has no coverage
//! marker until those scenarios drive observable browser behaviour.

use std::collections::VecDeque;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize};
use std::sync::{Arc, Mutex};

use deepseek_custom::application::actor::AppEvent;
use deepseek_custom::application::dto::{
    AppRevision, AppSnapshot, NoticeLevel, OperationKind, SessionSummary, TranscriptBlock,
    TranscriptContent, Workspace,
};
use deepseek_custom::application::services::{
    DomainCommandPort, RuntimeSettingsPort, SettingsController,
};
use deepseek_custom::application::test_control::{
    TestClock, TestExecution, TestExecutionError, TestExecutor, TestInvocation, TestOutputChunk,
    TestOutputStream, TestProcessExit,
};
use deepseek_custom::config::settings::Settings;
use deepseek_custom::voice::service::{VoiceCommand, VoiceEvent, VoiceState};
use deepseek_custom::web::server::{
    BindPolicy, NativeFolderPicker, WebAppState, WebServerHandle, start_with_policy_and_state,
};
use tempfile::TempDir;
use tokio::sync::mpsc;

const INSTALL_COMMAND: &str =
    "cargo run -p deepseek-custom-tests --example install_playwright_chromium";
const FOCUSED_COMMAND: &str =
    "cargo test -p deepseek-custom-tests --test it web_browser -- --test-threads=1";
const ARTIFACT_ROOT: &str = "target/playwright-artifacts";

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
}

impl TestExecutor for ScriptedTestExecutor {
    fn start(
        &self,
        _invocation: &TestInvocation,
    ) -> Result<Box<dyn TestExecution>, TestExecutionError> {
        Ok(Box::new(ScriptedTestExecution {
            output: Some(self.output.clone()),
            exit_code: self.exit_code,
        }))
    }
}

struct ScriptedTestExecution {
    output: Option<String>,
    exit_code: Option<i32>,
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
}

/// Owns every resource used by one browser test. Dropping the temporary root
/// cannot affect the checkout because the server only receives this path.
struct BrowserHarness {
    project_root: TempDir,
    state: WebAppState,
    backend: ScriptedBackend,
    voice_rx: mpsc::UnboundedReceiver<VoiceCommand>,
    server: WebServerHandle,
}

impl BrowserHarness {
    async fn start() -> Self {
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
        let discovery = Arc::new(ScriptedTestExecutor {
            output: "web_browser::scripted_case: test\n".into(),
            exit_code: Some(0),
        });
        let runner = Arc::new(ScriptedTestExecutor {
            output: "running 1 test\ntest web_browser::scripted_case ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n".into(),
            exit_code: Some(0),
        });
        let state = WebAppState::with_settings(
            isolated_snapshot(project_root.path()),
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
        Self {
            project_root,
            state,
            backend,
            voice_rx,
            server,
        }
    }

    async fn shutdown(self) {
        self.server.shutdown().await.unwrap();
    }
}

fn isolated_snapshot(project_root: &Path) -> AppSnapshot {
    let mut snapshot = super::web_server::visible_snapshot();
    snapshot.revision = AppRevision::INITIAL;
    snapshot.workspace = Workspace::Chat;
    snapshot.transcript.clear();
    snapshot.saved_sessions.clear();
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

#[test]
fn browser_harness_uses_unique_ephemeral_state_and_deterministic_services() {
    super::web_server::run_async_test(async {
        let mut first = BrowserHarness::start().await;
        let second = BrowserHarness::start().await;

        assert_ne!(first.project_root.path(), second.project_root.path());
        assert_ne!(first.server.url(), second.server.url());
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
        first
            .state
            .apply_voice_event(VoiceEvent::StateChanged(VoiceState::Listening))
            .unwrap();
        assert_eq!(first.state.snapshot().transcript.len(), 2);
        assert!(
            first
                .state
                .snapshot()
                .operations
                .iter()
                .any(|operation| operation.kind == OperationKind::Voice)
        );
        assert!(first.voice_rx.try_recv().is_err());

        first.shutdown().await;
        second.shutdown().await;
    });
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
#[ignore = "requires the version-matched Chromium runtime"]
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
        let title = result.unwrap_or_else(|error| {
            panic!(
                "Chromium for locked playwright-rs 0.17.0 / Playwright {} is unavailable: {error}\nInstall it with:\n{INSTALL_COMMAND}",
                playwright_rs::PLAYWRIGHT_VERSION
            )
        });
        assert_eq!(title, "DeepSeekCustom");
    });
}
