use std::convert::Infallible;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{DefaultBodyLimit, Multipart, Path as AxumPath, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use futures::stream::{self, Stream, StreamExt};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinHandle;

use crate::agent::events::{RoutedEvent, StreamEvent};
use crate::agent::repeat::RepeatCommand;
use crate::application::actor::{AppEvent, ApplicationActor, ChatLifecycle, Replay};
use crate::application::command_dispatcher::ApplicationCommandDispatcher;
use crate::application::dto::{
    AppChange, AppCommandRequest, AppCommandResult, AppRevision, AppSnapshot, SessionSummary,
    VisibleSettings,
};
use crate::application::services::DomainCommandPort;
use crate::application::services::SettingsController;
use crate::application::test_control::{
    CargoTestDiscoveryExecutor, CargoTestExecutor, SystemTestClock, TestClock, TestDiscoveryState,
    TestExecutor, TestResultStore, TestRunRequest,
};
use crate::application::test_run_coordinator::TestRunCoordinator;
use crate::config::settings::Settings;
use crate::image_bytes::attachment_from_image_bytes;
use crate::procedure::ProcedureCommand;
use crate::procedure::ProcedureProgress;
use crate::search::SearchCommand;
use crate::voice::service::{VoiceCommand, VoiceEvent, VoiceState};
use std::sync::atomic::AtomicBool;

const APPLICATION_SHELL: &str = "index.html";
pub const REQUEST_TOKEN_HEADER: &str = "x-deepseek-request-token";
pub const MAX_IMAGE_UPLOAD_BYTES: usize = 5 * 1024 * 1024;

#[derive(RustEmbed)]
#[folder = "src/web/assets"]
struct EmbeddedAssets;

/// Process-owned application state shared by HTTP requests and service event producers.
/// Browser connection lifetimes do not affect this owner.
#[derive(Clone)]
pub struct WebAppState {
    inner: Arc<WebAppStateInner>,
}

struct WebAppStateInner {
    actor: Mutex<ApplicationActor>,
    changes: broadcast::Sender<AppChange>,
    request_token: String,
    settings: Option<Arc<SettingsController>>,
    folder_picker: Option<Arc<dyn NativeFolderPicker>>,
    tests: Option<Mutex<TestService>>,
}

struct TestService {
    project_root: PathBuf,
    discovery: TestDiscoveryState,
    runs: TestRunCoordinator,
    store: TestResultStore,
    discovery_executor: Arc<dyn TestExecutor>,
    run_executor: Arc<dyn TestExecutor>,
    clock: Arc<dyn TestClock>,
}

impl WebAppState {
    pub fn new(snapshot: AppSnapshot, replay_capacity: usize) -> Self {
        let (changes, _) = broadcast::channel(replay_capacity.max(1));
        Self {
            inner: Arc::new(WebAppStateInner {
                actor: Mutex::new(ApplicationActor::new(snapshot, replay_capacity)),
                changes,
                request_token: uuid::Uuid::new_v4().simple().to_string(),
                settings: None,
                folder_picker: None,
                tests: None,
            }),
        }
    }

    pub fn with_settings(
        snapshot: AppSnapshot,
        replay_capacity: usize,
        settings: Arc<SettingsController>,
        folder_picker: Arc<dyn NativeFolderPicker>,
    ) -> Self {
        let (changes, _) = broadcast::channel(replay_capacity.max(1));
        let actor = ApplicationActor::new(snapshot, replay_capacity)
            .with_settings_controller(Arc::clone(&settings));
        Self {
            inner: Arc::new(WebAppStateInner {
                actor: Mutex::new(actor),
                changes,
                request_token: uuid::Uuid::new_v4().simple().to_string(),
                settings: Some(settings),
                folder_picker: Some(folder_picker),
                tests: None,
            }),
        }
    }

    pub fn with_test_control(mut self, project_root: PathBuf) -> Self {
        self = self.with_test_service(
            project_root,
            Arc::new(CargoTestDiscoveryExecutor),
            Arc::new(CargoTestExecutor),
            Arc::new(SystemTestClock),
        );
        self
    }

    pub fn with_test_service(
        mut self,
        project_root: PathBuf,
        discovery_executor: Arc<dyn TestExecutor>,
        run_executor: Arc<dyn TestExecutor>,
        clock: Arc<dyn TestClock>,
    ) -> Self {
        let store = TestResultStore::new(&project_root);
        let service = TestService {
            project_root,
            discovery: TestDiscoveryState::default(),
            runs: TestRunCoordinator::default(),
            store,
            discovery_executor,
            run_executor,
            clock,
        };
        Arc::get_mut(&mut self.inner)
            .expect("test service must be connected before state is shared")
            .tests = Some(Mutex::new(service));
        self
    }

    pub fn with_voice_port(mut self, voice: DomainCommandPort<VoiceCommand>) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("voice port must be connected before state is shared")
            .actor
            .get_mut()
            .unwrap()
            .connect_voice_port(voice);
        self
    }

    pub fn with_chat_lifecycle(mut self, chat: ChatLifecycle) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("chat lifecycle must be connected before state is shared")
            .actor
            .get_mut()
            .unwrap()
            .connect_chat_lifecycle(chat);
        self
    }

    pub fn with_autopilot_port(
        mut self,
        port: DomainCommandPort<RepeatCommand>,
        interrupt: Arc<AtomicBool>,
    ) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("autopilot port must be connected before state is shared")
            .actor
            .get_mut()
            .unwrap()
            .connect_autopilot(port, interrupt);
        self
    }

    pub fn with_search_port(
        mut self,
        port: DomainCommandPort<SearchCommand>,
        interrupt: Arc<AtomicBool>,
    ) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("search port must be connected before state is shared")
            .actor
            .get_mut()
            .unwrap()
            .connect_search(port, interrupt);
        self
    }

    pub fn with_procedure_port(
        mut self,
        port: DomainCommandPort<ProcedureCommand>,
        interrupt: Arc<AtomicBool>,
    ) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("procedure port must be connected before state is shared")
            .actor
            .get_mut()
            .unwrap()
            .connect_procedure(port, interrupt);
        self
    }

    pub fn snapshot(&self) -> AppSnapshot {
        self.inner.actor.lock().unwrap().snapshot().clone()
    }

    fn request_token(&self) -> &str {
        &self.inner.request_token
    }

    fn submit(&self, request: AppCommandRequest) -> AppCommandResult {
        let mut actor = self.inner.actor.lock().unwrap();
        let previous = actor.snapshot().revision;
        let mut dispatcher = ApplicationCommandDispatcher::new(&mut actor);
        let result = dispatcher.dispatch(request);
        if matches!(result, AppCommandResult::Applied { .. })
            && let Replay::Changes(changes) = actor.replay_after(previous)
        {
            for change in changes {
                let _ = self.inner.changes.send(change);
            }
        }
        result
    }

    pub fn apply_event(
        &self,
        event: AppEvent,
    ) -> Result<AppRevision, crate::application::dto::AppError> {
        let mut actor = self.inner.actor.lock().unwrap();
        let previous = actor.snapshot().revision;
        let revision = actor.apply_event(event)?;
        if let Replay::Changes(changes) = actor.replay_after(previous) {
            for change in changes {
                let _ = self.inner.changes.send(change);
            }
        }
        Ok(revision)
    }

    pub fn apply_operation_stream_event(
        &self,
        event: &StreamEvent,
    ) -> Option<Result<AppRevision, crate::application::dto::AppError>> {
        let mut actor = self.inner.actor.lock().unwrap();
        let previous = actor.snapshot().revision;
        let result = actor.apply_operation_stream_event(event)?;
        if result.is_ok()
            && let Replay::Changes(changes) = actor.replay_after(previous)
        {
            for change in changes {
                let _ = self.inner.changes.send(change);
            }
        }
        Some(result)
    }

    pub fn apply_routed_stream_event(
        &self,
        routed: RoutedEvent,
    ) -> Option<Result<AppRevision, crate::application::dto::AppError>> {
        let mut actor = self.inner.actor.lock().unwrap();
        let previous = actor.snapshot().revision;
        let result = actor.apply_routed_stream_event(routed)?;
        if result.is_ok()
            && let Replay::Changes(changes) = actor.replay_after(previous)
        {
            for change in changes {
                let _ = self.inner.changes.send(change);
            }
        }
        Some(result)
    }

    pub fn apply_procedure_progress(
        &self,
        event: &ProcedureProgress,
    ) -> Option<Result<AppRevision, crate::application::dto::AppError>> {
        let mut actor = self.inner.actor.lock().unwrap();
        let previous = actor.snapshot().revision;
        let result = actor.apply_procedure_progress(event)?;
        if result.is_ok()
            && let Replay::Changes(changes) = actor.replay_after(previous)
        {
            for change in changes {
                let _ = self.inner.changes.send(change);
            }
        }
        Some(result)
    }

    pub fn apply_voice_event(
        &self,
        event: VoiceEvent,
    ) -> Result<AppRevision, crate::application::dto::AppError> {
        let operation = match event {
            VoiceEvent::StateChanged(state) => crate::application::dto::OperationState {
                kind: crate::application::dto::OperationKind::Voice,
                operation_id: Some("voice-service".into()),
                phase: if state == VoiceState::Idle {
                    crate::application::dto::OperationPhase::Completed
                } else {
                    crate::application::dto::OperationPhase::Running
                },
                progress: None,
                message: Some(
                    match state {
                        VoiceState::Idle => "Voice ready",
                        VoiceState::Listening => "Listening",
                        VoiceState::Transcribing => "Transcribing",
                        VoiceState::Speaking => "Playing response",
                    }
                    .into(),
                ),
                error: None,
            },
            VoiceEvent::Transcript(text) => {
                return self.apply_event(AppEvent::TranscriptAppended(
                    crate::application::dto::TranscriptBlock {
                        id: self
                            .snapshot()
                            .transcript
                            .iter()
                            .map(|block| block.id)
                            .max()
                            .unwrap_or(0)
                            + 1,
                        content: crate::application::dto::TranscriptContent::Notice {
                            message: format!("Transcription: {text}"),
                            level: crate::application::dto::NoticeLevel::Info,
                        },
                    },
                ));
            }
            VoiceEvent::WakeDetected => {
                return self.apply_event(AppEvent::TranscriptAppended(
                    crate::application::dto::TranscriptBlock {
                        id: self
                            .snapshot()
                            .transcript
                            .iter()
                            .map(|block| block.id)
                            .max()
                            .unwrap_or(0)
                            + 1,
                        content: crate::application::dto::TranscriptContent::Notice {
                            message: "Wake phrase detected".into(),
                            level: crate::application::dto::NoticeLevel::Info,
                        },
                    },
                ));
            }
            VoiceEvent::Error(message) => {
                return self.apply_event(AppEvent::Error(crate::application::dto::AppError {
                    code: crate::application::dto::AppErrorCode::ServiceFailed,
                    message,
                    recoverable: true,
                    field: Some("voice".into()),
                }));
            }
        };
        self.apply_event(AppEvent::OperationChanged(operation))
    }

    fn replay_and_subscribe(
        &self,
        revision: AppRevision,
    ) -> (Replay, broadcast::Receiver<AppChange>) {
        let actor = self.inner.actor.lock().unwrap();
        let replay = actor.replay_after(revision);
        let receiver = self.inner.changes.subscribe();
        (replay, receiver)
    }

    fn snapshot_and_subscribe(&self) -> (AppSnapshot, broadcast::Receiver<AppChange>) {
        let actor = self.inner.actor.lock().unwrap();
        let snapshot = actor.snapshot().clone();
        let receiver = self.inner.changes.subscribe();
        (snapshot, receiver)
    }
}

pub trait NativeFolderPicker: Send + Sync + 'static {
    fn pick_folder(&self, initial_directory: &Path) -> io::Result<Option<PathBuf>>;
}

#[derive(Debug, Default)]
pub struct SystemFolderPicker;

impl NativeFolderPicker for SystemFolderPicker {
    fn pick_folder(&self, initial_directory: &Path) -> io::Result<Option<PathBuf>> {
        Ok(rfd::FileDialog::new()
            .set_directory(initial_directory)
            .pick_folder())
    }
}

impl Default for WebAppState {
    fn default() -> Self {
        let state = Self::new(
            AppSnapshot::initial(
                VisibleSettings::from_settings(&Settings::default(), None, None),
                SessionSummary {
                    id: String::new(),
                    title: "New conversation".into(),
                    backend: String::new(),
                    model: String::new(),
                },
            ),
            256,
        );
        match std::env::current_dir() {
            Ok(project_root) => state.with_test_control(project_root),
            Err(_) => state,
        }
    }
}

pub trait BrowserOpener: Send + Sync + 'static {
    fn open(&self, url: &str) -> io::Result<()>;
}

#[derive(Debug, Default)]
pub struct SystemBrowser;

impl BrowserOpener for SystemBrowser {
    fn open(&self, url: &str) -> io::Result<()> {
        open::that(url).map_err(io::Error::other)
    }
}

#[derive(Debug, Error)]
pub enum ServerStartError {
    #[error("web server address must be loopback, got {0}")]
    NonLoopback(SocketAddr),
    #[error("failed to bind web server to {address}: {source}")]
    Bind {
        address: SocketAddr,
        #[source]
        source: io::Error,
    },
    #[error("failed to determine bound web server address: {0}")]
    LocalAddress(#[source] io::Error),
    #[error("failed to open browser at {url}: {source}")]
    Browser {
        url: String,
        #[source]
        source: io::Error,
    },
}

#[derive(Debug, Error)]
pub enum ServerShutdownError {
    #[error("web server task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
    #[error("web server failed: {0}")]
    Serve(#[from] io::Error),
}

pub struct WebServerHandle {
    address: SocketAddr,
    url: String,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<io::Result<()>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BindPolicy {
    preferred_address: SocketAddr,
    fallback_to_ephemeral_port: bool,
}

impl BindPolicy {
    pub fn strict(address: SocketAddr) -> Self {
        Self {
            preferred_address: address,
            fallback_to_ephemeral_port: false,
        }
    }

    pub fn preferred_loopback(port: u16, fallback_to_ephemeral_port: bool) -> Self {
        Self {
            preferred_address: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
            fallback_to_ephemeral_port,
        }
    }

    pub fn preferred_address(self) -> SocketAddr {
        self.preferred_address
    }
}

impl WebServerHandle {
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub async fn shutdown(mut self) -> Result<(), ServerShutdownError> {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.task.await??;
        Ok(())
    }
}

pub async fn start(
    requested_address: SocketAddr,
    browser: Option<Arc<dyn BrowserOpener>>,
) -> Result<WebServerHandle, ServerStartError> {
    start_with_policy(BindPolicy::strict(requested_address), browser).await
}

pub async fn start_production(
    preferred_port: u16,
    fallback_to_ephemeral_port: bool,
    browser: Option<Arc<dyn BrowserOpener>>,
) -> Result<WebServerHandle, ServerStartError> {
    start_with_policy(
        BindPolicy::preferred_loopback(preferred_port, fallback_to_ephemeral_port),
        browser,
    )
    .await
}

pub async fn start_with_policy(
    policy: BindPolicy,
    browser: Option<Arc<dyn BrowserOpener>>,
) -> Result<WebServerHandle, ServerStartError> {
    start_with_policy_and_state(policy, browser, WebAppState::default()).await
}

pub async fn start_with_policy_and_state(
    policy: BindPolicy,
    browser: Option<Arc<dyn BrowserOpener>>,
    state: WebAppState,
) -> Result<WebServerHandle, ServerStartError> {
    let preferred_address = policy.preferred_address();
    if !preferred_address.ip().is_loopback() {
        return Err(ServerStartError::NonLoopback(preferred_address));
    }

    let listener = match TcpListener::bind(preferred_address).await {
        Ok(listener) => listener,
        Err(source) if !policy.fallback_to_ephemeral_port || preferred_address.port() == 0 => {
            return Err(ServerStartError::Bind {
                address: preferred_address,
                source,
            });
        }
        Err(_) => {
            let fallback_address = SocketAddr::new(preferred_address.ip(), 0);
            TcpListener::bind(fallback_address)
                .await
                .map_err(|source| ServerStartError::Bind {
                    address: fallback_address,
                    source,
                })?
        }
    };
    let address = listener
        .local_addr()
        .map_err(ServerStartError::LocalAddress)?;
    let url = format!("http://{address}/");

    if let Some(browser) = browser {
        browser
            .open(&url)
            .map_err(|source| ServerStartError::Browser {
                url: url.clone(),
                source,
            })?;
    }

    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let served_origin = url.clone();
    let task = tokio::spawn(async move {
        axum::serve(listener, router(state, served_origin))
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await
    });

    Ok(WebServerHandle {
        address,
        url,
        shutdown: Some(shutdown_tx),
        task,
    })
}

fn router(state: WebAppState, origin: String) -> Router {
    let security = WebSecurity {
        state: state.clone(),
        origin,
    };
    Router::new()
        .route("/health", get(health))
        .route("/api/health", get(health))
        .route("/api/bootstrap", get(bootstrap))
        .route("/api/snapshot", get(snapshot))
        .route("/api/events", get(events))
        .route("/api/commands", post(command))
        .route("/api/attachments", post(upload_attachment))
        .route("/api/attachments/{id}", delete(clear_attachment))
        .route("/api", get(api_not_found))
        .route("/api/{*path}", get(api_not_found))
        .fallback(get(fallback))
        .layer(DefaultBodyLimit::max(MAX_IMAGE_UPLOAD_BYTES + 64 * 1024))
        .layer(middleware::from_fn(security_headers))
        .with_state(security)
}

#[derive(Serialize)]
struct UploadedAttachment {
    attachment_id: String,
    media_type: String,
    size: usize,
}

async fn upload_attachment(
    State(security): State<WebSecurity>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    if !authorized(&security, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let field = match multipart.next_field().await {
        Ok(Some(field)) => field,
        Ok(None) => return (StatusCode::BAD_REQUEST, "image field is required").into_response(),
        Err(error) => return (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    };
    if field.name() != Some("image") {
        return (
            StatusCode::BAD_REQUEST,
            "multipart field must be named image",
        )
            .into_response();
    }
    let bytes = match field.bytes().await {
        Ok(bytes) if !bytes.is_empty() && bytes.len() <= MAX_IMAGE_UPLOAD_BYTES => bytes,
        Ok(_) => {
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                "image must be between 1 byte and 5 MiB",
            )
                .into_response();
        }
        Err(error) => return (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    };
    let attachment = match attachment_from_image_bytes(&bytes, "uploaded image") {
        Ok(attachment)
            if matches!(
                attachment.media_type.as_str(),
                "image/png" | "image/jpeg" | "image/bmp"
            ) =>
        {
            attachment
        }
        Ok(_) => {
            return (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "supported formats are PNG, JPEG, and BMP",
            )
                .into_response();
        }
        Err(message) => return (StatusCode::UNSUPPORTED_MEDIA_TYPE, message).into_response(),
    };
    let id = uuid::Uuid::new_v4().simple().to_string();
    let response = UploadedAttachment {
        attachment_id: id.clone(),
        media_type: attachment.media_type.clone(),
        size: bytes.len(),
    };
    let mut actor = security.state.inner.actor.lock().unwrap();
    actor.register_attachment(id.clone(), attachment);
    (StatusCode::CREATED, Json(response)).into_response()
}

async fn clear_attachment(
    State(security): State<WebSecurity>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> Response {
    if !authorized(&security, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if security
        .state
        .inner
        .actor
        .lock()
        .unwrap()
        .remove_attachment(&id)
    {
        StatusCode::NO_CONTENT.into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

fn authorized(security: &WebSecurity, headers: &HeaderMap) -> bool {
    headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        == Some(security.origin.trim_end_matches('/'))
        && headers
            .get(REQUEST_TOKEN_HEADER)
            .and_then(|value| value.to_str().ok())
            == Some(security.state.request_token())
}

#[derive(Clone)]
struct WebSecurity {
    state: WebAppState,
    origin: String,
}

async fn bootstrap(State(security): State<WebSecurity>) -> Response {
    let mut response = Json(security.state.snapshot()).into_response();
    response.headers_mut().insert(
        REQUEST_TOKEN_HEADER,
        HeaderValue::from_str(security.state.request_token())
            .expect("generated request tokens contain only header-safe ASCII"),
    );
    response
}

async fn snapshot(State(security): State<WebSecurity>) -> Json<AppSnapshot> {
    Json(security.state.snapshot())
}

async fn command(
    State(security): State<WebSecurity>,
    headers: HeaderMap,
    Json(request): Json<AppCommandRequest>,
) -> Response {
    if !authorized(&security, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }

    let result = if matches!(
        &request.command,
        crate::application::dto::AppCommand::PickWorkingDirectory
    ) {
        pick_working_directory(security.state.clone(), request.revision).await
    } else if matches!(
        &request.command,
        crate::application::dto::AppCommand::RefreshTests
            | crate::application::dto::AppCommand::StartTestRun { .. }
            | crate::application::dto::AppCommand::CancelTestRun
    ) {
        test_command(security.state.clone(), request).await
    } else {
        security.state.submit(request)
    };
    let status = match result {
        AppCommandResult::Applied { .. } => StatusCode::OK,
        AppCommandResult::Conflict { .. } => StatusCode::CONFLICT,
        AppCommandResult::Rejected { .. } => StatusCode::UNPROCESSABLE_ENTITY,
    };
    (status, Json(result)).into_response()
}

async fn test_command(state: WebAppState, request: AppCommandRequest) -> AppCommandResult {
    if state.snapshot().revision != request.revision {
        return AppCommandResult::Conflict {
            current_revision: state.snapshot().revision,
        };
    }
    let Some(tests) = &state.inner.tests else {
        return rejected_test_command("test control is not connected", None);
    };
    match request.command {
        crate::application::dto::AppCommand::RefreshTests => {
            let mut service = tests.lock().unwrap();
            let project_root = service.project_root.clone();
            let executor = Arc::clone(&service.discovery_executor);
            let clock = Arc::clone(&service.clock);
            let _ = service
                .discovery
                .refresh(&project_root, executor.as_ref(), clock.as_ref());
            publish_test_state(&state, &service)
        }
        crate::application::dto::AppCommand::StartTestRun { request } => {
            let mut service = tests.lock().unwrap();
            if let Err(message) = start_test_run(&mut service, &request) {
                return rejected_test_command(&message, Some("identity"));
            }
            let published = publish_test_state(&state, &service);
            drop(service);
            if matches!(published, AppCommandResult::Applied { .. }) {
                spawn_test_poll(state.clone());
            }
            published
        }
        crate::application::dto::AppCommand::CancelTestRun => {
            let mut service = tests.lock().unwrap();
            let clock = Arc::clone(&service.clock);
            if let Err(error) = service.runs.cancel(clock.as_ref()) {
                return rejected_test_command(&error.to_string(), None);
            }
            let _ = service.runs.retain_latest(&service.store);
            publish_test_state(&state, &service)
        }
        _ => unreachable!("test command was filtered by the route"),
    }
}

fn start_test_run(service: &mut TestService, request: &TestRunRequest) -> Result<(), String> {
    let project_root = service.project_root.clone();
    let executor = Arc::clone(&service.run_executor);
    let clock = Arc::clone(&service.clock);
    service
        .runs
        .start(
            &service.discovery,
            request,
            &project_root,
            executor.as_ref(),
            clock.as_ref(),
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn publish_test_state(state: &WebAppState, service: &TestService) -> AppCommandResult {
    let snapshot = service
        .runs
        .snapshot_with_results(&service.discovery, &service.store)
        .unwrap_or_else(|error| {
            let mut snapshot = service.runs.snapshot(&service.discovery);
            snapshot.retained_result_warnings.push(error.to_string());
            snapshot
        });
    match state.apply_event(AppEvent::TestsChanged(Box::new(snapshot))) {
        Ok(revision) => AppCommandResult::Applied { revision },
        Err(error) => AppCommandResult::Rejected { error },
    }
}

fn spawn_test_poll(state: WebAppState) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(75)).await;
            let Some(tests) = &state.inner.tests else {
                return;
            };
            let mut service = tests.lock().unwrap();
            let clock = Arc::clone(&service.clock);
            let terminal = match service.runs.poll(clock.as_ref()) {
                Ok(result) => result.is_some() && service.runs.active.is_none(),
                Err(_) => true,
            };
            if terminal {
                let _ = service.runs.retain_latest(&service.store);
            }
            let _ = publish_test_state(&state, &service);
            if terminal {
                return;
            }
        }
    });
}

fn rejected_test_command(message: &str, field: Option<&str>) -> AppCommandResult {
    AppCommandResult::Rejected {
        error: crate::application::dto::AppError {
            code: crate::application::dto::AppErrorCode::ServiceFailed,
            message: message.into(),
            recoverable: true,
            field: field.map(str::to_owned),
        },
    }
}

async fn pick_working_directory(state: WebAppState, revision: AppRevision) -> AppCommandResult {
    if state.snapshot().revision != revision {
        return AppCommandResult::Conflict {
            current_revision: state.snapshot().revision,
        };
    }
    let (Some(settings), Some(picker)) = (
        state.inner.settings.clone(),
        state.inner.folder_picker.clone(),
    ) else {
        return AppCommandResult::Rejected {
            error: crate::application::dto::AppError {
                code: crate::application::dto::AppErrorCode::Unavailable,
                message: "native folder picker is not connected".into(),
                recoverable: true,
                field: Some("working_dir".into()),
            },
        };
    };
    let initial = settings.working_dir();
    let selected = match tokio::task::spawn_blocking(move || picker.pick_folder(&initial)).await {
        Ok(Ok(selected)) => selected,
        Ok(Err(error)) => return folder_error(error.to_string()),
        Err(error) => return folder_error(error.to_string()),
    };
    let Some(selected) = selected else {
        return AppCommandResult::Applied { revision };
    };
    if state.snapshot().revision != revision {
        return AppCommandResult::Conflict {
            current_revision: state.snapshot().revision,
        };
    }
    match settings.set_working_dir(selected) {
        Ok(visible) => match state.apply_event(AppEvent::SettingsChanged(visible)) {
            Ok(revision) => AppCommandResult::Applied { revision },
            Err(error) => AppCommandResult::Rejected { error },
        },
        Err(error) => AppCommandResult::Rejected { error },
    }
}

fn folder_error(message: String) -> AppCommandResult {
    AppCommandResult::Rejected {
        error: crate::application::dto::AppError {
            code: crate::application::dto::AppErrorCode::ServiceFailed,
            message,
            recoverable: true,
            field: Some("working_dir".into()),
        },
    }
}

#[derive(Deserialize)]
struct EventsQuery {
    after: Option<AppRevision>,
}

async fn events(
    State(security): State<WebSecurity>,
    Query(query): Query<EventsQuery>,
    headers: HeaderMap,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let header_revision = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(AppRevision);
    let revision = query.after.or(header_revision).unwrap_or_default();
    let (replay, receiver) = security.state.replay_and_subscribe(revision);
    let initial = match replay {
        Replay::Changes(changes) => changes,
        Replay::Reset(snapshot) => vec![AppChange {
            revision: snapshot.revision,
            change: crate::application::dto::AppChangeKind::Reset(snapshot),
        }],
    };
    let initial = stream::iter(initial.into_iter().map(sse_event));
    let live_state = security.state.clone();
    let live = stream::unfold((receiver, live_state), |(mut receiver, state)| async move {
        match receiver.recv().await {
            Ok(change) => Some((sse_event(change), (receiver, state))),
            Err(broadcast::error::RecvError::Lagged(_)) => {
                let (snapshot, fresh_receiver) = state.snapshot_and_subscribe();
                let reset = AppChange {
                    revision: snapshot.revision,
                    change: crate::application::dto::AppChangeKind::Reset(Box::new(snapshot)),
                };
                Some((sse_event(reset), (fresh_receiver, state)))
            }
            Err(broadcast::error::RecvError::Closed) => None,
        }
    });
    Sse::new(initial.chain(live)).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keepalive"),
    )
}

fn sse_event(change: AppChange) -> Result<Event, Infallible> {
    let event_name = if matches!(
        change.change,
        crate::application::dto::AppChangeKind::Reset(_)
    ) {
        "reset"
    } else {
        "change"
    };
    Ok(Event::default()
        .event(event_name)
        .id(change.revision.0.to_string())
        .json_data(change)
        .expect("application changes are serializable"))
}

async fn health() -> &'static str {
    "ok"
}

async fn security_headers(request: axum::extract::Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'",
        ),
    );
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    response
}

async fn api_not_found() -> StatusCode {
    StatusCode::NOT_FOUND
}

async fn fallback(uri: Uri) -> Response {
    let requested_asset = uri.path().trim_start_matches('/');
    if !requested_asset.is_empty()
        && let Some(asset) = EmbeddedAssets::get(requested_asset)
    {
        return (
            [(header::CONTENT_TYPE, asset_content_type(requested_asset))],
            asset.data,
        )
            .into_response();
    }

    match EmbeddedAssets::get(APPLICATION_SHELL) {
        Some(asset) => (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            asset.data,
        )
            .into_response(),
        None => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Html("missing application shell"),
        )
            .into_response(),
    }
}

fn asset_content_type(path: &str) -> &'static str {
    match Path::new(path).extension().and_then(|value| value.to_str()) {
        Some("css") => "text/css; charset=utf-8",
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("wasm") => "application/wasm",
        _ => "application/octet-stream",
    }
}
