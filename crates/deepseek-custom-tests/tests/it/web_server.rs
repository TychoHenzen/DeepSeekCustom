use std::collections::VecDeque;
use std::io;
#[cfg(windows)]
use std::io::{BufRead, BufReader};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
#[cfg(windows)]
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize};
use std::sync::{Arc, Mutex};
#[cfg(windows)]
use std::time::{Duration, Instant};

use deepseek_custom::application::actor::AppEvent;
use deepseek_custom::application::dto::{
    AppCommand, AppCommandRequest, AppCommandResult, AppRevision, AppSnapshot, NoticeLevel,
    OperationKind, OperationPhase, OperationProgress, OperationState, PendingSessionSwitch,
    SessionSummary, TranscriptBlock, TranscriptContent, VisibleBackend, VisibleProcedureSettings,
    VisibleSettings, VisibleStyleSettings, VisibleVoiceSettings, Workspace,
};
use deepseek_custom::application::services::DomainCommandPort;
use deepseek_custom::application::services::{RuntimeSettingsPort, SettingsController};
use deepseek_custom::application::test_control::{
    TestClock, TestExecution, TestExecutionError, TestExecutor, TestInvocation, TestOutputChunk,
    TestOutputStream, TestProcessExit, TestRunRequest,
};
use deepseek_custom::config::settings::Settings;
use deepseek_custom::voice::service::{VoiceCommand, VoiceEvent, VoiceState};
use deepseek_custom::web::server::{
    BindPolicy, BrowserOpener, NativeFolderPicker, REQUEST_TOKEN_HEADER, ServerStartError,
    WebAppState, start, start_production, start_with_policy, start_with_policy_and_state,
};

struct FixedFolderPicker(Mutex<VecDeque<Option<std::path::PathBuf>>>);
impl NativeFolderPicker for FixedFolderPicker {
    fn pick_folder(
        &self,
        _initial_directory: &std::path::Path,
    ) -> io::Result<Option<std::path::PathBuf>> {
        Ok(self.0.lock().unwrap().pop_front().unwrap())
    }
}

#[derive(Default)]
struct RecordingBrowser {
    urls: Mutex<Vec<String>>,
}

struct WebTestClock;
impl TestClock for WebTestClock {
    fn now_ms(&self) -> u64 {
        44
    }
}

struct WebTestExecutor {
    output: String,
}
impl TestExecutor for WebTestExecutor {
    fn start(
        &self,
        _invocation: &TestInvocation,
    ) -> Result<Box<dyn TestExecution>, TestExecutionError> {
        Ok(Box::new(WebTestExecution {
            output: Some(self.output.clone()),
        }))
    }
}
struct WebTestExecution {
    output: Option<String>,
}
impl TestExecution for WebTestExecution {
    fn next_output(&mut self) -> Result<Option<TestOutputChunk>, TestExecutionError> {
        Ok(self.output.take().map(|text| TestOutputChunk {
            sequence: 0,
            stream: TestOutputStream::Stdout,
            text,
        }))
    }
    fn try_wait(&mut self) -> Result<Option<TestProcessExit>, TestExecutionError> {
        Ok(self
            .output
            .is_none()
            .then_some(TestProcessExit { exit_code: Some(0) }))
    }
    fn cancel_and_wait(&mut self) -> Result<TestProcessExit, TestExecutionError> {
        Ok(TestProcessExit { exit_code: None })
    }
}

impl BrowserOpener for RecordingBrowser {
    fn open(&self, url: &str) -> io::Result<()> {
        self.urls.lock().unwrap().push(url.to_owned());
        Ok(())
    }
}

fn ephemeral_loopback() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)
}

// covers: deepseek-custom/web-application :: The application starts as a local web service :: Normal production startup
#[test]
fn lifecycle_reports_url_serves_health_and_fallback_then_releases_port() {
    run_async_test(async {
        let browser = Arc::new(RecordingBrowser::default());
        let server = start_production(0, true, Some(browser.clone()))
            .await
            .unwrap();

        assert!(server.address().ip().is_loopback());
        assert_ne!(server.address().port(), 0);
        assert_eq!(server.url(), format!("http://{}/", server.address()));
        assert_eq!(browser.urls.lock().unwrap().as_slice(), [server.url()]);

        let client = reqwest::Client::new();
        let health = client
            .get(format!("{}health", server.url()))
            .send()
            .await
            .unwrap();
        assert_eq!(health.status(), reqwest::StatusCode::OK);
        assert_eq!(health.text().await.unwrap(), "ok");

        let api_health = client
            .get(format!("{}api/health", server.url()))
            .send()
            .await
            .unwrap();
        assert_eq!(api_health.status(), reqwest::StatusCode::OK);
        assert_eq!(api_health.text().await.unwrap(), "ok");

        let fallback = client
            .get(format!("{}not-an-api-route", server.url()))
            .send()
            .await
            .unwrap();
        assert_eq!(fallback.status(), reqwest::StatusCode::OK);
        assert_eq!(
            fallback.headers()[reqwest::header::CONTENT_TYPE],
            "text/html; charset=utf-8"
        );
        assert!(fallback.text().await.unwrap().contains("DeepSeekCustom"));

        let missing_api = client
            .get(format!("{}api/not-implemented", server.url()))
            .send()
            .await
            .unwrap();
        assert_eq!(missing_api.status(), reqwest::StatusCode::NOT_FOUND);

        let address = server.address();
        server.shutdown().await.unwrap();

        let rebound = tokio::net::TcpListener::bind(address).await.unwrap();
        drop(rebound);
    });
}

#[test]
fn production_shell_references_assets_that_the_rust_server_serves() {
    run_async_test(async {
        let server = start_production(0, true, None).await.unwrap();
        let client = reqwest::Client::new();
        let shell = client
            .get(server.url())
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();

        let script_path = html_attribute(&shell, "src=\"");
        let stylesheet_path = html_attribute(&shell, "href=\"");
        assert!(script_path.starts_with("/assets/"), "{script_path}");
        assert!(stylesheet_path.starts_with("/assets/"), "{stylesheet_path}");

        let script = client
            .get(format!(
                "{}{}",
                server.url(),
                script_path.trim_start_matches('/')
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(script.status(), reqwest::StatusCode::OK);
        assert_eq!(
            script.headers()[reqwest::header::CONTENT_TYPE],
            "text/javascript; charset=utf-8"
        );
        assert!(
            script
                .text()
                .await
                .unwrap()
                .contains("Skip to active workspace")
        );

        let stylesheet = client
            .get(format!(
                "{}{}",
                server.url(),
                stylesheet_path.trim_start_matches('/')
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(stylesheet.status(), reqwest::StatusCode::OK);
        assert_eq!(
            stylesheet.headers()[reqwest::header::CONTENT_TYPE],
            "text/css; charset=utf-8"
        );
        let stylesheet = stylesheet.text().await.unwrap();
        assert!(stylesheet.contains("@media (width<=48rem)"));
        assert!(stylesheet.contains("overflow-x:hidden"));

        server.shutdown().await.unwrap();
    });
}

fn html_attribute<'a>(html: &'a str, prefix: &str) -> &'a str {
    let value = html
        .split_once(prefix)
        .unwrap_or_else(|| panic!("missing {prefix:?} in production shell"))
        .1;
    value
        .split_once('"')
        .unwrap_or_else(|| panic!("unterminated {prefix:?} in production shell"))
        .0
}

#[tokio::test]
async fn browser_open_can_be_suppressed() {
    let server = start(ephemeral_loopback(), None).await.unwrap();
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn non_loopback_address_is_rejected_before_binding() {
    let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0);
    let error = match start(address, None).await {
        Ok(server) => {
            server.shutdown().await.unwrap();
            panic!("non-loopback address unexpectedly started")
        }
        Err(error) => error,
    };

    assert!(matches!(error, ServerStartError::NonLoopback(actual) if actual == address));
}

// covers: deepseek-custom/web-application :: The application starts as a local web service :: Preferred port is unavailable
#[test]
fn unavailable_preferred_port_falls_back_to_a_reported_loopback_origin() {
    run_async_test(async {
        let occupied = tokio::net::TcpListener::bind(ephemeral_loopback())
            .await
            .unwrap();
        let preferred = occupied.local_addr().unwrap();

        let server = start_production(preferred.port(), true, None)
            .await
            .unwrap();

        assert!(server.address().ip().is_loopback());
        assert_ne!(server.address().port(), preferred.port());
        assert_eq!(server.url(), format!("http://{}/", server.address()));
        let response = reqwest::get(server.url()).await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert!(response.text().await.unwrap().contains("DeepSeekCustom"));

        server.shutdown().await.unwrap();
    });
}

// covers: deepseek-custom/web-application :: The application starts as a local web service :: Preferred port is unavailable
#[test]
fn strict_preferred_port_failure_preserves_the_os_bind_error() {
    run_async_test(async {
        let occupied = tokio::net::TcpListener::bind(ephemeral_loopback())
            .await
            .unwrap();
        let preferred = occupied.local_addr().unwrap();
        let expected = tokio::net::TcpListener::bind(preferred).await.unwrap_err();

        let error = match start_production(preferred.port(), false, None).await {
            Ok(server) => {
                server.shutdown().await.unwrap();
                panic!("occupied preferred port unexpectedly started")
            }
            Err(error) => error,
        };

        match error {
            ServerStartError::Bind { address, source } => {
                assert_eq!(address, preferred);
                assert_eq!(source.kind(), expected.kind());
                assert_eq!(source.raw_os_error(), expected.raw_os_error());
                assert_eq!(source.to_string(), expected.to_string());
            }
            other => panic!("expected bind error, got {other}"),
        }
    });
}

#[tokio::test]
async fn non_loopback_policy_never_creates_a_listener() {
    let probe = tokio::net::TcpListener::bind(ephemeral_loopback())
        .await
        .unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    let non_loopback = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port);

    let error = match start_with_policy(BindPolicy::strict(non_loopback), None).await {
        Ok(server) => {
            server.shutdown().await.unwrap();
            panic!("non-loopback policy unexpectedly started")
        }
        Err(error) => error,
    };
    assert!(matches!(error, ServerStartError::NonLoopback(actual) if actual == non_loopback));

    let exclusive_probe = tokio::net::TcpListener::bind(non_loopback).await.unwrap();
    drop(exclusive_probe);
}

pub(super) fn run_async_test(future: impl std::future::Future<Output = ()>) {
    tokio::runtime::Runtime::new().unwrap().block_on(future);
}

pub(super) fn visible_snapshot() -> AppSnapshot {
    AppSnapshot {
        revision: AppRevision::INITIAL,
        workspace: Workspace::Procedure,
        transcript: vec![TranscriptBlock {
            id: 7,
            content: TranscriptContent::Notice {
                message: "visible transcript".into(),
                level: NoticeLevel::Info,
            },
        }],
        session: SessionSummary {
            id: "session-current".into(),
            title: "Current session".into(),
            backend: "stub".into(),
            model: "deterministic".into(),
        },
        saved_sessions: Vec::new(),
        pending_session_switch: Some(PendingSessionSwitch::Load("session-next".into())),
        settings: VisibleSettings {
            backends: vec![VisibleBackend {
                name: "stub".into(),
                configured_model: "deterministic".into(),
                models: vec!["deterministic".into()],
            }],
            selected_backend: Some("stub".into()),
            selected_model: Some("deterministic".into()),
            effort: "high".into(),
            context_budget: 4096,
            show_raw_output: true,
            max_tokens: 4096,
            working_dir: Some("C:/workspace".into()),
            style: VisibleStyleSettings {
                plain_language: true,
                target_grade: 8.0,
            },
            voice: VisibleVoiceSettings {
                enabled: true,
                stt_enabled: true,
                tts_enabled: false,
                trigger_mode: "push_to_talk".into(),
                wake_phrase: "computer".into(),
                tts_voice: "af_sarah".into(),
                tts_speed: 1.0,
            },
            procedure: VisibleProcedureSettings {
                localization_backend: None,
                local_patch_backend: None,
                frontier_patch_backend: None,
                index_max_files: 10_000,
                index_max_total_bytes: 64 * 1024 * 1024,
                verifier_commands: Vec::new(),
            },
        },
        operations: vec![operation(
            OperationKind::Procedure,
            OperationPhase::AwaitingReview,
            1,
        )],
        tests: Default::default(),
    }
}

fn operation(kind: OperationKind, phase: OperationPhase, completed: u64) -> OperationState {
    OperationState {
        kind,
        operation_id: Some(format!("{kind:?}-run")),
        phase,
        progress: Some(OperationProgress {
            completed,
            total: Some(2),
        }),
        message: Some(format!("{phase:?}")),
        error: None,
    }
}

async fn start_state(state: WebAppState) -> deepseek_custom::web::server::WebServerHandle {
    start_with_policy_and_state(BindPolicy::strict(ephemeral_loopback()), None, state)
        .await
        .unwrap()
}

async fn read_sse_event(mut response: reqwest::Response) -> String {
    let mut body = String::new();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !body.contains("\n\n") {
            let bytes = response.chunk().await.unwrap().unwrap();
            body.push_str(std::str::from_utf8(&bytes).unwrap());
        }
    })
    .await
    .unwrap();
    body
}

#[cfg(windows)]
fn process_is_alive(pid: u32) -> bool {
    let output = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .expect("tasklist should run");
    String::from_utf8_lossy(&output.stdout).contains(&pid.to_string())
}

#[cfg(windows)]
fn run_process_shutdown_probe() -> u32 {
    let mut probe = Command::new(env!("CARGO_BIN_EXE_orphan_probe"))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("process-shutdown probe should start");
    let mut line = String::new();
    BufReader::new(probe.stdout.take().expect("piped probe stdout"))
        .read_line(&mut line)
        .expect("probe should report its owned child");
    assert!(probe.wait().expect("probe should exit").success());
    line.trim()
        .parse()
        .unwrap_or_else(|_| panic!("expected child process id, got {line:?}"))
}

async fn request_token(client: &reqwest::Client, server_url: &str) -> String {
    client
        .get(format!("{server_url}api/bootstrap"))
        .send()
        .await
        .unwrap()
        .headers()[REQUEST_TOKEN_HEADER]
        .to_str()
        .unwrap()
        .to_owned()
}

async fn post_command(
    client: &reqwest::Client,
    server_url: &str,
    token: &str,
    request: &AppCommandRequest,
) -> reqwest::Response {
    client
        .post(format!("{server_url}api/commands"))
        .header(reqwest::header::ORIGIN, server_url.trim_end_matches('/'))
        .header(REQUEST_TOKEN_HEADER, token)
        .json(request)
        .send()
        .await
        .unwrap()
}

// covers: deepseek-custom/web-application :: Settings preserve runtime and persistence boundaries :: User requests a working-directory folder
#[test]
fn native_folder_request_changes_only_working_dir_and_cancel_is_a_no_op() {
    run_async_test(async {
        let root = super::scratch_dir("web-folder-picker", "confirm-cancel");
        let selected = root.join("selected");
        std::fs::create_dir_all(&selected).unwrap();
        let working = Arc::new(Mutex::new(root.clone()));
        let runtime = RuntimeSettingsPort::new(
            root.clone(),
            Arc::new(AtomicU8::new(0)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicUsize::new(100_000)),
            Arc::new(Mutex::new("model".into())),
            Arc::clone(&working),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU8::new(8)),
        );
        let controller = Arc::new(SettingsController::new(
            root.clone(),
            Settings::default(),
            runtime,
            None,
            None,
        ));
        let picker = Arc::new(FixedFolderPicker(Mutex::new(VecDeque::from([
            Some(selected.clone()),
            None,
        ]))));
        let state =
            WebAppState::with_settings(visible_snapshot(), 8, Arc::clone(&controller), picker);
        let server = start_state(state.clone()).await;
        let client = reqwest::Client::new();
        let token = request_token(&client, server.url()).await;

        let confirmed: AppCommandResult = post_command(
            &client,
            server.url(),
            &token,
            &AppCommandRequest {
                revision: state.snapshot().revision,
                command: AppCommand::PickWorkingDirectory,
            },
        )
        .await
        .json()
        .await
        .unwrap();
        assert!(matches!(confirmed, AppCommandResult::Applied { .. }));
        assert_eq!(*working.lock().unwrap(), selected);
        assert_eq!(controller.project_root(), root.as_path());
        let persisted_after_confirm = std::fs::read_to_string(root.join("settings.json")).unwrap();

        let cancelled: AppCommandResult = post_command(
            &client,
            server.url(),
            &token,
            &AppCommandRequest {
                revision: state.snapshot().revision,
                command: AppCommand::PickWorkingDirectory,
            },
        )
        .await
        .json()
        .await
        .unwrap();
        assert!(matches!(cancelled, AppCommandResult::Applied { .. }));
        assert_eq!(*working.lock().unwrap(), selected);
        assert_eq!(
            std::fs::read_to_string(root.join("settings.json")).unwrap(),
            persisted_after_confirm
        );
        server.shutdown().await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    });
}

// covers: deepseek-custom/web-application :: Browser state reflects one authoritative application state :: Browser connects during an idle session
#[test]
fn bootstrap_and_reload_return_the_complete_current_visible_snapshot() {
    run_async_test(async {
        let state = WebAppState::new(visible_snapshot(), 8);
        state
            .apply_event(AppEvent::OperationChanged(operation(
                OperationKind::Procedure,
                OperationPhase::Completed,
                2,
            )))
            .unwrap();
        let expected = state.snapshot();
        let server = start_state(state.clone()).await;
        let client = reqwest::Client::new();

        let bootstrap = client
            .get(format!("{}api/bootstrap", server.url()))
            .send()
            .await
            .unwrap();
        assert_eq!(bootstrap.status(), reqwest::StatusCode::OK);
        let bootstrap_text = bootstrap.text().await.unwrap();
        let bootstrap: AppSnapshot = serde_json::from_str(&bootstrap_text).unwrap();
        let reload: AppSnapshot = client
            .get(format!("{}api/snapshot", server.url()))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();

        assert_eq!(bootstrap, expected);
        assert_eq!(reload, expected);
        assert_eq!(bootstrap.revision, AppRevision(1));
        assert_eq!(bootstrap.workspace, Workspace::Procedure);
        assert_eq!(bootstrap.transcript.len(), 1);
        assert_eq!(bootstrap.session.id, "session-current");
        assert_eq!(
            bootstrap.pending_session_switch,
            Some(PendingSessionSwitch::Load("session-next".into()))
        );
        assert_eq!(
            bootstrap.settings.working_dir.as_deref(),
            Some("C:/workspace")
        );
        assert_eq!(bootstrap.operations[0].phase, OperationPhase::Completed);
        assert!(!bootstrap_text.contains("api_key"));
        assert!(!bootstrap_text.contains("credential"));
        assert!(!bootstrap_text.contains("secret"));

        server.shutdown().await.unwrap();
    });
}

// covers: deepseek-custom/web-application :: Browser state reflects one authoritative application state :: Browser reconnects during active work
#[test]
fn reconnect_replays_each_active_operation_once_and_resets_evicted_history() {
    run_async_test(async {
        let state = WebAppState::new(visible_snapshot(), 2);
        let server = start_state(state.clone()).await;
        let client = reqwest::Client::new();
        let kinds = [
            OperationKind::Chat,
            OperationKind::Cascade,
            OperationKind::Evolve,
            OperationKind::Procedure,
            OperationKind::Autopilot,
            OperationKind::Voice,
            OperationKind::Tests,
        ];

        for kind in kinds {
            let before = state.snapshot().revision;
            let running_revision = state
                .apply_event(AppEvent::OperationChanged(operation(
                    kind,
                    OperationPhase::Running,
                    1,
                )))
                .unwrap();
            let disconnected = client
                .get(format!("{}api/events?after={}", server.url(), before.0))
                .send()
                .await
                .unwrap();
            let first = read_sse_event(disconnected).await;
            assert!(first.contains("event: change"), "{kind:?}: {first}");
            assert!(first.contains(&format!("id: {}", running_revision.0)));
            assert_eq!(
                first
                    .matches(&format!("id: {}", running_revision.0))
                    .count(),
                1
            );

            let completed_revision = state
                .apply_event(AppEvent::OperationChanged(operation(
                    kind,
                    OperationPhase::Completed,
                    2,
                )))
                .unwrap();
            let reconnected = client
                .get(format!("{}api/events", server.url()))
                .header("Last-Event-ID", running_revision.0)
                .send()
                .await
                .unwrap();
            let later = read_sse_event(reconnected).await;
            assert!(later.contains(&format!("id: {}", completed_revision.0)));
            assert!(!later.contains(&format!("id: {}\n", running_revision.0)));
            assert_eq!(
                state
                    .snapshot()
                    .operations
                    .iter()
                    .find(|operation| operation.kind == kind)
                    .unwrap()
                    .phase,
                OperationPhase::Completed
            );
        }

        let old_revision = AppRevision::INITIAL;
        let reset = client
            .get(format!(
                "{}api/events?after={}",
                server.url(),
                old_revision.0
            ))
            .send()
            .await
            .unwrap();
        let reset_event = read_sse_event(reset).await;
        assert!(reset_event.contains("event: reset"));
        assert!(reset_event.contains(&format!("id: {}", state.snapshot().revision.0)));
        let snapshot: AppSnapshot = client
            .get(format!("{}api/snapshot", server.url()))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(snapshot, state.snapshot());
        assert_eq!(snapshot.operations.len(), kinds.len());

        server.shutdown().await.unwrap();
    });
}

#[cfg(windows)]
#[test]
fn production_server_milestone_survives_reload_resets_and_reaps_process_resources() {
    run_async_test(async {
        let browser = Arc::new(RecordingBrowser::default());
        let state = WebAppState::new(visible_snapshot(), 2);
        let server = start_with_policy_and_state(
            BindPolicy::preferred_loopback(0, true),
            Some(browser.clone()),
            state.clone(),
        )
        .await
        .unwrap();
        let address = server.address();
        let url = server.url().to_owned();
        assert_eq!(browser.urls.lock().unwrap().as_slice(), [url.clone()]);

        let client = reqwest::Client::new();
        let health = client.get(format!("{url}api/health")).send().await.unwrap();
        assert_eq!(health.status(), reqwest::StatusCode::OK);
        assert_eq!(health.text().await.unwrap(), "ok");

        let running_revision = state
            .apply_event(AppEvent::OperationChanged(operation(
                OperationKind::Procedure,
                OperationPhase::Running,
                1,
            )))
            .unwrap();
        let first_connection = client
            .get(format!("{url}api/events?after=0"))
            .send()
            .await
            .unwrap();
        let running_event = read_sse_event(first_connection).await;
        assert!(running_event.contains("event: change"));
        assert!(running_event.contains(&format!("id: {}", running_revision.0)));

        let reloaded: AppSnapshot = client
            .get(format!("{url}api/snapshot"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(reloaded.revision, running_revision);
        assert_eq!(reloaded.operations[0].phase, OperationPhase::Running);

        state
            .apply_event(AppEvent::OperationChanged(operation(
                OperationKind::Procedure,
                OperationPhase::Running,
                2,
            )))
            .unwrap();
        let completed_revision = state
            .apply_event(AppEvent::OperationChanged(operation(
                OperationKind::Procedure,
                OperationPhase::Completed,
                2,
            )))
            .unwrap();
        let reset_connection = client
            .get(format!("{url}api/events?after=0"))
            .send()
            .await
            .unwrap();
        let reset_event = read_sse_event(reset_connection).await;
        assert!(reset_event.contains("event: reset"));
        assert!(reset_event.contains(&format!("id: {}", completed_revision.0)));

        server.shutdown().await.unwrap();
        let rebound = tokio::net::TcpListener::bind(address).await.unwrap();
        drop(rebound);

        let child_pid = run_process_shutdown_probe();
        let deadline = Instant::now() + Duration::from_secs(10);
        while process_is_alive(child_pid) && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if process_is_alive(child_pid) {
            let _ = Command::new("taskkill")
                .args(["/PID", &child_pid.to_string(), "/T", "/F"])
                .output();
            panic!("owned child {child_pid} outlived process shutdown");
        }
    });
}

// covers: deepseek-custom/web-application :: Browser state reflects one authoritative application state :: A stale client sends a command
#[test]
fn stale_command_returns_atomic_conflict_without_mutating_state() {
    run_async_test(async {
        let state = WebAppState::new(visible_snapshot(), 8);
        let server = start_state(state.clone()).await;
        let client = reqwest::Client::new();
        let token = request_token(&client, server.url()).await;

        let applied = post_command(
            &client,
            server.url(),
            &token,
            &AppCommandRequest {
                revision: AppRevision::INITIAL,
                command: AppCommand::SelectWorkspace {
                    workspace: Workspace::Chat,
                },
            },
        )
        .await;
        assert_eq!(applied.status(), reqwest::StatusCode::OK);
        assert_eq!(
            applied.json::<AppCommandResult>().await.unwrap(),
            AppCommandResult::Applied {
                revision: AppRevision(1)
            }
        );
        let before_conflict = state.snapshot();

        let stale = post_command(
            &client,
            server.url(),
            &token,
            &AppCommandRequest {
                revision: AppRevision::INITIAL,
                command: AppCommand::SelectWorkspace {
                    workspace: Workspace::Settings,
                },
            },
        )
        .await;
        assert_eq!(stale.status(), reqwest::StatusCode::CONFLICT);
        assert_eq!(
            stale.json::<AppCommandResult>().await.unwrap(),
            AppCommandResult::Conflict {
                current_revision: AppRevision(1)
            }
        );
        assert_eq!(state.snapshot(), before_conflict);

        server.shutdown().await.unwrap();
    });
}

// covers: deepseek-custom/web-application :: Local web commands are protected from other origins :: Same-origin command is valid
#[test]
fn same_origin_current_token_dispatches_through_normal_application_rules() {
    run_async_test(async {
        let first_state = WebAppState::new(visible_snapshot(), 8);
        let first = start_state(first_state.clone()).await;
        let second = start_state(WebAppState::new(visible_snapshot(), 8)).await;
        let client = reqwest::Client::new();
        let first_token = request_token(&client, first.url()).await;
        let second_token = request_token(&client, second.url()).await;
        assert_ne!(first_token, second_token);
        assert_eq!(first_token.len(), 32);

        let applied = post_command(
            &client,
            first.url(),
            &first_token,
            &AppCommandRequest {
                revision: AppRevision::INITIAL,
                command: AppCommand::SelectWorkspace {
                    workspace: Workspace::Tests,
                },
            },
        )
        .await;
        assert_eq!(applied.status(), reqwest::StatusCode::OK);
        assert_eq!(first_state.snapshot().workspace, Workspace::Tests);

        let chat = post_command(
            &client,
            first.url(),
            &first_token,
            &AppCommandRequest {
                revision: AppRevision(1),
                command: AppCommand::SendMessage {
                    text: "hello".into(),
                    attachment_id: None,
                },
            },
        )
        .await;
        assert_eq!(chat.status(), reqwest::StatusCode::OK);
        assert!(matches!(
            chat.json::<AppCommandResult>().await.unwrap(),
            AppCommandResult::Applied {
                revision: AppRevision(3)
            }
        ));
        assert_eq!(first_state.snapshot().revision, AppRevision(3));
        assert!(matches!(
            first_state.snapshot().transcript.last(),
            Some(TranscriptBlock { content: TranscriptContent::User { text, has_image: false }, .. }) if text == "hello"
        ));

        first.shutdown().await.unwrap();
        second.shutdown().await.unwrap();
    });
}

// covers: deepseek-custom/web-application :: Local web commands are protected from other origins :: Another origin attempts a command
#[test]
fn untrusted_command_shapes_fail_without_dispatch_or_data_disclosure() {
    run_async_test(async {
        let state = WebAppState::new(visible_snapshot(), 8);
        let server = start_state(state.clone()).await;
        let client = reqwest::Client::new();
        let token = request_token(&client, server.url()).await;
        let endpoint = format!("{}api/commands", server.url());
        let body = serde_json::to_string(&AppCommandRequest {
            revision: AppRevision::INITIAL,
            command: AppCommand::SelectWorkspace {
                workspace: Workspace::Settings,
            },
        })
        .unwrap();

        let responses = vec![
            client
                .post(&endpoint)
                .header(REQUEST_TOKEN_HEADER, &token)
                .json(&serde_json::from_str::<serde_json::Value>(&body).unwrap())
                .send()
                .await
                .unwrap(),
            client
                .post(&endpoint)
                .header(reqwest::header::ORIGIN, "https://attacker.invalid")
                .header(REQUEST_TOKEN_HEADER, &token)
                .body(body.clone())
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .send()
                .await
                .unwrap(),
            client
                .post(&endpoint)
                .header(reqwest::header::ORIGIN, server.url().trim_end_matches('/'))
                .json(&serde_json::from_str::<serde_json::Value>(&body).unwrap())
                .send()
                .await
                .unwrap(),
            client
                .post(&endpoint)
                .header(reqwest::header::ORIGIN, server.url().trim_end_matches('/'))
                .header(REQUEST_TOKEN_HEADER, "wrong-token")
                .json(&serde_json::from_str::<serde_json::Value>(&body).unwrap())
                .send()
                .await
                .unwrap(),
            client
                .request(reqwest::Method::OPTIONS, &endpoint)
                .header(reqwest::header::ORIGIN, "https://attacker.invalid")
                .header(reqwest::header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
                .send()
                .await
                .unwrap(),
            client
                .post(&endpoint)
                .header(reqwest::header::ORIGIN, server.url().trim_end_matches('/'))
                .header(REQUEST_TOKEN_HEADER, &token)
                .header(reqwest::header::CONTENT_TYPE, "text/plain")
                .body(body.clone())
                .send()
                .await
                .unwrap(),
            client
                .put(&endpoint)
                .header(reqwest::header::ORIGIN, server.url().trim_end_matches('/'))
                .header(REQUEST_TOKEN_HEADER, &token)
                .json(&serde_json::from_str::<serde_json::Value>(&body).unwrap())
                .send()
                .await
                .unwrap(),
        ];

        for response in responses {
            assert!(!response.status().is_success());
            assert!(
                response
                    .headers()
                    .get(reqwest::header::ACCESS_CONTROL_ALLOW_ORIGIN)
                    .is_none()
            );
            assert_eq!(response.headers()["x-frame-options"], "DENY");
            assert_eq!(response.headers()["x-content-type-options"], "nosniff");
            assert_eq!(response.headers()["referrer-policy"], "no-referrer");
            assert!(
                response.headers()["content-security-policy"]
                    .to_str()
                    .unwrap()
                    .contains("frame-ancestors 'none'")
            );
            let response_body = response.text().await.unwrap();
            for forbidden in [
                "visible transcript",
                "session-current",
                "api_key",
                "credential",
                "secret",
                &token,
            ] {
                assert!(
                    !response_body.contains(forbidden),
                    "leaked {forbidden:?}: {response_body}"
                );
            }
            assert_eq!(state.snapshot().revision, AppRevision::INITIAL);
            assert_eq!(state.snapshot().workspace, Workspace::Procedure);
        }

        let bootstrap = client
            .get(format!("{}api/bootstrap", server.url()))
            .send()
            .await
            .unwrap();
        let bootstrap_body = bootstrap.text().await.unwrap();
        assert!(!bootstrap_body.contains(&token));
        assert!(!bootstrap_body.contains("api_key"));
        assert!(!bootstrap_body.contains("credential"));
        assert!(!bootstrap_body.contains("secret"));

        server.shutdown().await.unwrap();
    });
}

#[test]
fn tests_workspace_commands_refresh_start_and_publish_terminal_reconnect_state() {
    run_async_test(async {
        let root = std::env::temp_dir().join(format!(
            "deepseek-web-tests-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let state = WebAppState::new(visible_snapshot(), 16).with_test_service(
            root.clone(),
            Arc::new(WebTestExecutor { output: "application_actor::updates: test\n".into() }),
            Arc::new(WebTestExecutor { output: "running 1 test\ntest application_actor::updates ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n".into() }),
            Arc::new(WebTestClock),
        );
        let server = start_state(state.clone()).await;
        let client = reqwest::Client::new();
        let token = request_token(&client, server.url()).await;

        let refreshed = post_command(
            &client,
            server.url(),
            &token,
            &AppCommandRequest {
                revision: AppRevision::INITIAL,
                command: AppCommand::RefreshTests,
            },
        )
        .await;
        assert_eq!(refreshed.status(), reqwest::StatusCode::OK);
        let snapshot = state.snapshot();
        let catalogue = snapshot.tests.discovery.catalogue.unwrap();
        let identity = catalogue.modules[0].tests[0].clone();
        let started = post_command(
            &client,
            server.url(),
            &token,
            &AppCommandRequest {
                revision: snapshot.revision,
                command: AppCommand::StartTestRun {
                    request: TestRunRequest {
                        identity: identity.clone(),
                        catalogue_revision: catalogue.discovered_at_ms,
                    },
                },
            },
        )
        .await;
        assert_eq!(started.status(), reqwest::StatusCode::OK);

        for _ in 0..30 {
            if state
                .snapshot()
                .tests
                .retained_results
                .iter()
                .any(|result| result.identity == identity)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        let reconnected: AppSnapshot = client
            .get(format!("{}api/snapshot", server.url()))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let result = reconnected
            .tests
            .retained_results
            .iter()
            .find(|result| result.identity == identity)
            .unwrap();
        assert_eq!(result.counts.passed, 1);
        assert_eq!(
            result.output.lines().last().unwrap(),
            "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out"
        );
        assert!(reconnected.tests.active.is_none());
        server.shutdown().await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    });
}

// covers: deepseek-custom/web-application :: Attachments and voice controls remain usable :: User attaches an image
#[test]
fn supported_image_upload_is_bounded_previewable_and_consumed_by_chat_submission() {
    run_async_test(async {
        let state = WebAppState::new(visible_snapshot(), 8);
        let server = start_state(state.clone()).await;
        let client = reqwest::Client::new();
        let token = request_token(&client, server.url()).await;
        let png = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=",
        ).unwrap();
        let response = client
            .post(format!("{}api/attachments", server.url()))
            .header("origin", server.url().trim_end_matches('/'))
            .header(REQUEST_TOKEN_HEADER, &token)
            .multipart(
                reqwest::multipart::Form::new().part(
                    "image",
                    reqwest::multipart::Part::bytes(png.clone())
                        .file_name("pixel.png")
                        .mime_str("image/png")
                        .unwrap(),
                ),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::CREATED);
        let uploaded: serde_json::Value = response.json().await.unwrap();
        assert_eq!(uploaded["media_type"], "image/png");
        assert_eq!(uploaded["size"], png.len());
        let attachment_id = uploaded["attachment_id"].as_str().unwrap().to_owned();

        let result: AppCommandResult = post_command(
            &client,
            server.url(),
            &token,
            &AppCommandRequest {
                revision: state.snapshot().revision,
                command: AppCommand::SendMessage {
                    text: "inspect".into(),
                    attachment_id: Some(attachment_id.clone()),
                },
            },
        )
        .await
        .json()
        .await
        .unwrap();
        assert!(matches!(result, AppCommandResult::Applied { .. }));
        assert!(matches!(
            state.snapshot().transcript.last(),
            Some(TranscriptBlock {
                content: TranscriptContent::User {
                    has_image: true,
                    ..
                },
                ..
            })
        ));

        let reused: AppCommandResult = post_command(
            &client,
            server.url(),
            &token,
            &AppCommandRequest {
                revision: state.snapshot().revision,
                command: AppCommand::SendMessage {
                    text: "reuse".into(),
                    attachment_id: Some(attachment_id),
                },
            },
        )
        .await
        .json()
        .await
        .unwrap();
        assert!(matches!(reused, AppCommandResult::Rejected { .. }));
        server.shutdown().await.unwrap();
    });
}

// covers: deepseek-custom/web-application :: Attachments and voice controls remain usable :: User uses push to talk
#[test]
fn focused_push_to_talk_commands_and_voice_events_share_the_rust_voice_service_state() {
    run_async_test(async {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let state =
            WebAppState::new(visible_snapshot(), 8).with_voice_port(DomainCommandPort::new(tx));
        let server = start_state(state.clone()).await;
        let client = reqwest::Client::new();
        let token = request_token(&client, server.url()).await;
        for command in [AppCommand::StartVoiceCapture, AppCommand::StopVoiceCapture] {
            let result: AppCommandResult = post_command(
                &client,
                server.url(),
                &token,
                &AppCommandRequest {
                    revision: state.snapshot().revision,
                    command,
                },
            )
            .await
            .json()
            .await
            .unwrap();
            assert!(matches!(result, AppCommandResult::Applied { .. }));
        }
        assert_eq!(rx.recv().await, Some(VoiceCommand::StartListening));
        assert_eq!(rx.recv().await, Some(VoiceCommand::StopListening));

        state
            .apply_voice_event(VoiceEvent::StateChanged(VoiceState::Speaking))
            .unwrap();
        state
            .apply_voice_event(VoiceEvent::Transcript("spoken words".into()))
            .unwrap();
        state
            .apply_voice_event(VoiceEvent::Error("microphone unavailable".into()))
            .unwrap();
        let snapshot = state.snapshot();
        assert_eq!(
            snapshot
                .operations
                .iter()
                .find(|item| item.kind == OperationKind::Voice)
                .unwrap()
                .message
                .as_deref(),
            Some("Playing response")
        );
        assert!(snapshot.transcript.iter().any(|block| matches!(&block.content, TranscriptContent::Notice { message, .. } if message == "Transcription: spoken words")));
        server.shutdown().await.unwrap();
    });
}
