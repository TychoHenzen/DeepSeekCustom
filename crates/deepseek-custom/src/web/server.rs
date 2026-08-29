use std::convert::Infallible;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::stream::{self, Stream, StreamExt};
use rust_embed::RustEmbed;
use serde::Deserialize;
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinHandle;

use crate::application::actor::{AppEvent, ApplicationActor, Replay};
use crate::application::dto::{
    AppChange, AppCommandRequest, AppCommandResult, AppRevision, AppSnapshot, SessionSummary,
    VisibleSettings,
};
use crate::config::settings::Settings;

const APPLICATION_SHELL: &str = "index.html";
pub const REQUEST_TOKEN_HEADER: &str = "x-deepseek-request-token";

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
}

impl WebAppState {
    pub fn new(snapshot: AppSnapshot, replay_capacity: usize) -> Self {
        let (changes, _) = broadcast::channel(replay_capacity.max(1));
        Self {
            inner: Arc::new(WebAppStateInner {
                actor: Mutex::new(ApplicationActor::new(snapshot, replay_capacity)),
                changes,
                request_token: uuid::Uuid::new_v4().simple().to_string(),
            }),
        }
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
        let result = actor.submit(request);
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

impl Default for WebAppState {
    fn default() -> Self {
        Self::new(
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
        )
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
        .route("/api", get(api_not_found))
        .route("/api/{*path}", get(api_not_found))
        .fallback(get(fallback))
        .layer(middleware::from_fn(security_headers))
        .with_state(security)
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
    if headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        != Some(security.origin.trim_end_matches('/'))
        || headers
            .get(REQUEST_TOKEN_HEADER)
            .and_then(|value| value.to_str().ok())
            != Some(security.state.request_token())
    {
        return StatusCode::FORBIDDEN.into_response();
    }

    let result = security.state.submit(request);
    let status = match result {
        AppCommandResult::Applied { .. } => StatusCode::OK,
        AppCommandResult::Conflict { .. } => StatusCode::CONFLICT,
        AppCommandResult::Rejected { .. } => StatusCode::UNPROCESSABLE_ENTITY,
    };
    (status, Json(result)).into_response()
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
