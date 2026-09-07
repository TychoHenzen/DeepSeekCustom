//! Tests proving the Ollama model-discovery path: HTTP fetch, controller
//! caching, and web-layer plumbing.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize};
use std::sync::{Arc, Mutex};

use deepseek_custom::api::models::{list_models, parse_ollama_tags};
use deepseek_custom::application::services::{RuntimeSettingsPort, SettingsController};
use deepseek_custom::config::settings::{ApiProvider, BackendConfig, Settings};
use deepseek_custom::web::server::{NativeFolderPicker, WebAppState};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const MOCK_TAGS: &str =
    r#"{"models":[{"name":"qwen2.5:1.5b"},{"name":"qwen2.5-coder:7b-instruct-q4_K_M"}]}"#;
const MOCK_MODELS: [&str; 2] = ["qwen2.5:1.5b", "qwen2.5-coder:7b-instruct-q4_K_M"];

fn ollama_entry(base_url: Option<&str>) -> BackendConfig {
    BackendConfig::Api {
        provider: ApiProvider::Ollama,
        model: "qwen2.5:1.5b".to_string(),
        base_url: base_url.map(str::to_string),
        api_key: None,
        models: None,
    }
}

async fn start_tags_mock() -> MockServer {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/tags"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(MOCK_TAGS, "application/json"))
        .mount(&mock)
        .await;
    mock
}

fn make_runtime(root: PathBuf) -> RuntimeSettingsPort {
    RuntimeSettingsPort::new(
        root.clone(),
        Arc::new(AtomicU8::new(0)),
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicUsize::new(100_000)),
        Arc::new(Mutex::new("qwen2.5:1.5b".to_string())),
        Arc::new(Mutex::new(root)),
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicU8::new(8)),
    )
}

fn make_controller(root: PathBuf, mock_uri: String) -> SettingsController {
    let settings = Settings {
        backends: Some(HashMap::from([(
            "ollama".to_string(),
            BackendConfig::Api {
                provider: ApiProvider::Ollama,
                model: "qwen2.5:1.5b".to_string(),
                base_url: Some(mock_uri),
                api_key: None,
                models: None,
            },
        )])),
        default_backend: Some("ollama".to_string()),
        ..Settings::default()
    };
    SettingsController::new(
        root.clone(),
        settings,
        make_runtime(root),
        Some("ollama".to_string()),
        Some("qwen2.5:1.5b".to_string()),
    )
}

struct NoopPicker;
impl NativeFolderPicker for NoopPicker {
    fn pick_folder(&self, _: &std::path::Path) -> std::io::Result<Option<std::path::PathBuf>> {
        Ok(None)
    }
}

// covers: HTTP fetch path in query_ollama_models.
#[tokio::test]
async fn ollama_list_models_fetches_from_api_tags_endpoint() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/tags"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(MOCK_TAGS, "application/json"))
        .expect(1)
        .mount(&mock)
        .await;

    let models = list_models(&ollama_entry(Some(&mock.uri()))).await;

    assert_eq!(models, MOCK_MODELS);
    mock.verify().await;
}

// covers: live Ollama server when present; fallback contract when absent.
#[tokio::test]
async fn ollama_live_discovery_returns_installed_models_or_fallback() {
    let probe = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .unwrap()
        .get("http://localhost:11434/api/tags")
        .send()
        .await;

    let models = list_models(&ollama_entry(None)).await;

    match probe {
        Ok(resp) if resp.status().is_success() => {
            let live_names = parse_ollama_tags(&resp.text().await.unwrap());
            assert_eq!(
                models, live_names,
                "discovered models must match what the live server reports"
            );
        }
        _ => {
            assert_eq!(
                models,
                ["qwen2.5:1.5b"],
                "fallback must be the declared model when Ollama is unreachable"
            );
        }
    }
}

// covers: refresh_models() populates model_options; update() retains it;
// set_working_dir() retains it. All three call self.visible() post-mutation.
#[tokio::test]
async fn settings_controller_ollama_refresh_caches_models_and_mutations_retain_them() {
    let mock = start_tags_mock().await;
    let root = super::scratch_dir("ollama-discovery", "controller");
    std::fs::create_dir_all(&root).unwrap();
    let controller = make_controller(root.clone(), mock.uri());

    assert_eq!(controller.visible().backends[0].models, ["qwen2.5:1.5b"]);

    let discovered = controller.refresh_models().await;
    assert_eq!(discovered.backends[0].models, MOCK_MODELS);

    let mut to_save = discovered;
    to_save.show_raw_output = true;
    let saved = controller.update(to_save).unwrap();
    assert_eq!(
        saved.backends[0].models, MOCK_MODELS,
        "update() must retain discovered models through model_options cache"
    );

    let after_dir = controller.set_working_dir(root.clone()).unwrap();
    assert_eq!(
        after_dir.backends[0].models, MOCK_MODELS,
        "set_working_dir() must retain discovered models through model_options cache"
    );

    std::fs::remove_dir_all(root).unwrap();
}

// covers: WebAppState::refresh_models() with an Ollama backend - revision
// increments and snapshot settings reflect discovered model list.
#[test]
fn ollama_model_discovery_through_web_state_bumps_revision_and_updates_snapshot() {
    use super::web_server::{run_async_test, visible_snapshot};
    run_async_test(async {
        let mock = start_tags_mock().await;
        let root = super::scratch_dir("ollama-discovery", "web-state");
        std::fs::create_dir_all(&root).unwrap();
        let controller = Arc::new(make_controller(root.clone(), mock.uri()));
        let state =
            WebAppState::with_settings(visible_snapshot(), 8, controller, Arc::new(NoopPicker));
        let initial_revision = state.snapshot().revision;
        assert_eq!(state.snapshot().settings.backends[0].models.len(), 1);

        let refreshed = state.refresh_models().await.unwrap();

        assert!(refreshed > initial_revision);
        assert_eq!(state.snapshot().settings.backends[0].models, MOCK_MODELS);
        std::fs::remove_dir_all(root).unwrap();
    });
}
