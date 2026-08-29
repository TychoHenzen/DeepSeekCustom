use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use deepseek_custom::web::server::{
    BindPolicy, BrowserOpener, ServerStartError, start, start_production, start_with_policy,
};

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

fn run_async_test(future: impl std::future::Future<Output = ()>) {
    tokio::runtime::Runtime::new().unwrap().block_on(future);
}
