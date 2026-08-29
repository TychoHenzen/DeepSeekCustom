use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use rust_embed::RustEmbed;
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const FALLBACK_ASSET: &str = "fallback.html";

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
    if !requested_address.ip().is_loopback() {
        return Err(ServerStartError::NonLoopback(requested_address));
    }

    let listener = TcpListener::bind(requested_address)
        .await
        .map_err(|source| ServerStartError::Bind {
            address: requested_address,
            source,
        })?;
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

async fn fallback() -> Response {
    match EmbeddedAssets::get(FALLBACK_ASSET) {
        Some(asset) => (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            asset.data,
        )
            .into_response(),
        None => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Html("missing fallback asset"),
        )
            .into_response(),
    }
}
