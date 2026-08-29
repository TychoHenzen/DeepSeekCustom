use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::http::{StatusCode, Uri, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use rust_embed::RustEmbed;
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const APPLICATION_SHELL: &str = "index.html";

#[derive(RustEmbed)]
#[folder = "src/web/assets"]
struct EmbeddedAssets;

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
    let task = tokio::spawn(async move {
        axum::serve(listener, router())
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

fn router() -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/api/health", get(health))
        .route("/api", get(api_not_found))
        .route("/api/{*path}", get(api_not_found))
        .fallback(get(fallback))
}

async fn health() -> &'static str {
    "ok"
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
