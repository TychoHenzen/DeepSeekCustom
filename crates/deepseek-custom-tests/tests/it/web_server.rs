use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use deepseek_custom::web::server::{BrowserOpener, ServerStartError, start};

#[derive(Default)]
struct RecordingBrowser {
    urls: Mutex<Vec<String>>,
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

#[tokio::test]
async fn lifecycle_reports_url_serves_health_and_fallback_then_releases_port() {
    let browser = Arc::new(RecordingBrowser::default());
    let server = start(ephemeral_loopback(), Some(browser.clone()))
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
