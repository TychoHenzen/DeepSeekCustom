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
    AppRevision, AppSnapshot, NoticeLevel, OperationKind, OperationPhase, OperationProgress,
    OperationState, SessionSummary, TranscriptBlock, TranscriptContent, Workspace,
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

use playwright_rs::protocol::{AriaRole, GetByRoleOptions, Locator, Page};

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
            server,
        }
    }

    async fn shutdown(self) {
        self.server.shutdown().await.unwrap();
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
            self.page.get_by_role(AriaRole::Status, None),
        )
        .await
    }

    fn action(&self, name: &str) -> Locator {
        self.page.get_by_role(
            AriaRole::Button,
            Some(GetByRoleOptions::default().name(name).exact(true)),
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
