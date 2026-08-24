use std::collections::HashMap;
use std::path::Path;

use deepseek_custom::api::client::ApiClient;
use deepseek_custom::api::provider::Provider;
use deepseek_custom::api::types::{ChatRequest, Content, Message, Role};
use deepseek_custom::config::settings::{ApiProvider, BackendConfig, ProcedureSettings, Settings};
use deepseek_custom::effort::Effort;
use deepseek_custom::procedure::{
    ContractSelection, LocalizationDispatcher, LocalizationPromptInput, ProcedureScratchpad,
    ProcedureTask, ProposalScope, RepositoryIndexEntry, RequirementSlice, ScenarioSlice,
    SelectedContractSlice, localization_response_format,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const LOCALIZATION_RESPONSE: &str = r#"{
  "id": "localization-1",
  "object": "chat.completion",
  "created": 1,
  "model": "qwen-local",
  "choices": [{
    "index": 0,
    "message": {"role": "assistant", "content": "{\"targets\":[]}"},
    "finish_reason": "stop"
  }],
  "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
}"#;

const TYPED_LOCALIZATION_RESPONSE: &str = r#"{
  "id": "localization-2",
  "object": "chat.completion",
  "created": 2,
  "model": "qwen-local",
  "choices": [{
    "index": 0,
    "message": {
      "role": "assistant",
      "content": "{\"targets\":[{\"path\":\"src/lib.rs\",\"symbol\":\"run\",\"evidence\":\"run owns the entry point\"}]}",
      "reasoning_content": "{\"targets\":[{\"path\":\"wrong/reasoning.rs\",\"symbol\":null,\"evidence\":\"must not decode\"}]}"
    },
    "finish_reason": "stop"
  }],
  "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
}"#;

const MALFORMED_LOCALIZATION_RESPONSE: &str = r#"{
  "id": "localization-3",
  "object": "chat.completion",
  "created": 3,
  "model": "qwen-local",
  "choices": [{
    "index": 0,
    "message": {"role": "assistant", "content": "not-json"},
    "finish_reason": "stop"
  }],
  "usage": {"prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12}
}"#;

fn localization_request(index: &[RepositoryIndexEntry]) -> ChatRequest {
    ChatRequest {
        model: "qwen-local".to_string(),
        messages: vec![Message {
            role: Role::User,
            content: Some(Content::text("localize this contract")),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }],
        tools: None,
        tool_choice: None,
        stream: false,
        temperature: Some(0.0),
        max_tokens: Some(1024),
        thinking: None,
        thinking_mode: None,
        reasoning_effort: None,
        response_format: Some(localization_response_format(index)),
        effort: Some(Effort::Low),
    }
}

fn settings_for(backend_name: &str, backend: BackendConfig) -> Settings {
    Settings {
        procedure: Some(ProcedureSettings {
            localization_backend: Some(backend_name.to_string()),
            ..ProcedureSettings::default()
        }),
        backends: Some(HashMap::from([(backend_name.to_string(), backend)])),
        effort: Some(Effort::High),
        max_tokens: Some(4096),
        ..Settings::default()
    }
}

fn selected_contract() -> SelectedContractSlice {
    SelectedContractSlice {
        change_id: "add-procedure-localization-runner".to_string(),
        task: ProcedureTask {
            id: "4.4".to_string(),
            text: "Dispatch one localization request".to_string(),
            covers: None,
        },
        proposal_scope: ProposalScope {
            why: "Localize one selected procedure task.".to_string(),
            what_changes: "Add a schema-constrained Ollama call.".to_string(),
        },
        selection: ContractSelection::Bound {
            capability: "deepseek-custom/procedure-localization".to_string(),
            requirement: RequirementSlice {
                name: "Localizer output is schema constrained".to_string(),
                text: "The localizer SHALL use structured output.".to_string(),
                scenarios: vec![ScenarioSlice {
                    name: "Ollama receives the localization schema".to_string(),
                    text: "- **WHEN** localization runs\n- **THEN** Ollama receives the schema"
                        .to_string(),
                }],
            },
        },
    }
}

fn repository_index() -> Vec<RepositoryIndexEntry> {
    vec![RepositoryIndexEntry {
        path: "src/lib.rs".to_string(),
        symbols: vec!["run".to_string()],
    }]
}

#[tokio::test]
async fn ollama_wire_request_carries_the_localization_json_schema() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_raw(LOCALIZATION_RESPONSE, "application/json"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let index = vec![
        RepositoryIndexEntry {
            path: "crates/deepseek-custom/src/procedure/run.rs".to_string(),
            symbols: vec!["LocalizationEnvelope".to_string()],
        },
        RepositoryIndexEntry {
            path: "docs/IntelligenceProcedure.md".to_string(),
            symbols: Vec::new(),
        },
    ];
    let client = ApiClient::new(Provider::Ollama, "ollama".to_string(), Some(server.uri()));

    client.chat(&localization_request(&index)).await.unwrap();

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let body: serde_json::Value = requests[0].body_json().unwrap();
    let format = &body["response_format"];
    assert_eq!(format["type"], "json_schema");
    assert_eq!(format["json_schema"]["name"], "procedure_localization");
    assert_eq!(format["json_schema"]["strict"], true);
    let schema = &format["json_schema"]["schema"];
    assert_eq!(schema["required"], serde_json::json!(["targets"]));
    assert_eq!(
        schema["properties"]["targets"]["items"]["required"],
        serde_json::json!(["path", "evidence"])
    );
    assert_eq!(
        schema["properties"]["targets"]["items"]["properties"]["path"]["enum"],
        serde_json::json!([
            "crates/deepseek-custom/src/procedure/run.rs",
            "docs/IntelligenceProcedure.md"
        ])
    );
    assert!(
        schema["properties"]["targets"]["items"]["properties"]
            .get("symbol")
            .is_some()
    );
    server.verify().await;
}

#[test]
fn ollama_localization_backend_passes_preflight() {
    let settings = settings_for(
        "local-ollama",
        BackendConfig::Api {
            provider: ApiProvider::Ollama,
            model: "qwen-local".to_string(),
            base_url: Some("http://localhost:11434/v1".to_string()),
            api_key: None,
            models: None,
        },
    );

    let dispatcher = LocalizationDispatcher::from_settings(&settings, Path::new("."))
        .expect("Ollama should pass localization preflight");

    assert_eq!(dispatcher.backend_name(), "local-ollama");
    assert_eq!(dispatcher.model(), "qwen-local");
}

#[tokio::test]
async fn deepseek_api_localization_backend_is_rejected_before_wire_dispatch() {
    let server = MockServer::start().await;
    let settings = settings_for(
        "remote-api",
        BackendConfig::Api {
            provider: ApiProvider::DeepSeek,
            model: "deepseek-v4-pro".to_string(),
            base_url: Some(server.uri()),
            api_key: Some("test-key".to_string()),
            models: None,
        },
    );

    let error = LocalizationDispatcher::from_settings(&settings, Path::new("."))
        .err()
        .expect("DeepSeek should fail localization preflight");

    assert_eq!(
        error.to_string(),
        "localization backend \"remote-api\" is unsupported: kind api provider deepseek cannot enforce the localization JSON Schema"
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[test]
fn claude_cli_localization_backend_is_rejected_before_process_dispatch() {
    let settings = settings_for(
        "claude-localizer",
        BackendConfig::ClaudeCli {
            model: "opus".to_string(),
            permission_mode: None,
            env: None,
            models: None,
        },
    );

    let error = LocalizationDispatcher::from_settings(&settings, Path::new("."))
        .err()
        .expect("Claude CLI should fail localization preflight");

    assert_eq!(
        error.to_string(),
        "localization backend \"claude-localizer\" is unsupported: kind claude_cli cannot enforce the localization JSON Schema"
    );
}

#[test]
fn codex_cli_localization_backend_is_rejected_before_process_dispatch() {
    let settings = settings_for(
        "codex-localizer",
        BackendConfig::CodexCli {
            model: "gpt-5.6-sol".to_string(),
            sandbox: None,
            env: None,
            models: None,
        },
    );

    let error = LocalizationDispatcher::from_settings(&settings, Path::new("."))
        .err()
        .expect("Codex CLI should fail localization preflight");

    assert_eq!(
        error.to_string(),
        "localization backend \"codex-localizer\" is unsupported: kind codex_cli cannot enforce the localization JSON Schema"
    );
}

#[tokio::test]
async fn localization_dispatch_is_one_tool_free_non_streaming_ollama_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_raw(TYPED_LOCALIZATION_RESPONSE, "application/json"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let settings = settings_for(
        "local-ollama",
        BackendConfig::Api {
            provider: ApiProvider::Ollama,
            model: "qwen-local".to_string(),
            base_url: Some(server.uri()),
            api_key: None,
            models: None,
        },
    );
    let dispatcher = LocalizationDispatcher::from_settings(&settings, Path::new(".")).unwrap();
    let contract = selected_contract();
    let index = repository_index();
    let scratchpad = ProcedureScratchpad {
        goals: vec!["find the implementation entry point".to_string()],
        files: Vec::new(),
        changes: Vec::new(),
        last_error: None,
    };

    let envelope = dispatcher
        .localize(LocalizationPromptInput {
            contract: &contract,
            repository_index: &index,
            scratchpad: &scratchpad,
        })
        .await
        .unwrap();

    assert_eq!(envelope.targets.len(), 1);
    assert_eq!(envelope.targets[0].path, "src/lib.rs");
    assert_eq!(envelope.targets[0].symbol.as_deref(), Some("run"));
    assert_eq!(envelope.targets[0].evidence, "run owns the entry point");

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let body: serde_json::Value = requests[0].body_json().unwrap();
    assert_eq!(body["model"], "qwen-local");
    assert_eq!(body["stream"], false);
    assert_eq!(body["temperature"], 0.0);
    assert_eq!(body["max_tokens"], 4096);
    assert_eq!(body["reasoning_effort"], "high");
    assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    assert_eq!(body["messages"][0]["role"], "user");
    assert!(
        body["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("Localizer output is schema constrained")
    );
    assert!(body.get("tools").is_none());
    assert!(body.get("tool_choice").is_none());
    assert_eq!(body["response_format"]["type"], "json_schema");
    server.verify().await;
}

#[tokio::test]
async fn malformed_final_content_returns_one_deterministic_error_without_retry() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_raw(MALFORMED_LOCALIZATION_RESPONSE, "application/json"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let settings = settings_for(
        "local-ollama",
        BackendConfig::Api {
            provider: ApiProvider::Ollama,
            model: "qwen-local".to_string(),
            base_url: Some(server.uri()),
            api_key: None,
            models: None,
        },
    );
    let dispatcher = LocalizationDispatcher::from_settings(&settings, Path::new(".")).unwrap();
    let contract = selected_contract();
    let index = repository_index();
    let scratchpad = ProcedureScratchpad::default();

    let error = dispatcher
        .localize(LocalizationPromptInput {
            contract: &contract,
            repository_index: &index,
            scratchpad: &scratchpad,
        })
        .await
        .unwrap_err();

    assert_eq!(
        error.to_string(),
        "localization response final content is not a valid LocalizationEnvelope: expected ident at line 1 column 2"
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    server.verify().await;
}
