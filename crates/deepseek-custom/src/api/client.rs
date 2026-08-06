use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use tokio::sync::mpsc;
use tokio::time::sleep;
use tracing::{debug, info, warn};

use crate::api::types::*;
use crate::error::{HarnessError, Result};

/// Which backend an `ApiClient` talks to. Both providers accept the same
/// OpenAI-compatible request shape at `{base_url}/chat/completions`, so one
/// client type serves both. `prepare_request` adapts the request per
/// provider before it goes out.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Provider {
    DeepSeek,
    Ollama,
}

/// Client for an OpenAI-compatible chat completions API (DeepSeek or Ollama).
pub struct ApiClient {
    client: reqwest::Client,
    provider: Provider,
    base_url: String,
    api_key: String,
    default_model: String,
    max_retries: u32,
    base_delay_ms: u64,
}

impl ApiClient {
    /// Create a new ApiClient.
    ///
    /// `api_key` is required. `base_url` defaults per provider when `None`:
    /// `https://api.deepseek.com` for `Provider::DeepSeek`, and
    /// `http://localhost:11434/v1` for `Provider::Ollama`. `default_model`
    /// defaults to `"deepseek-v4-flash"`.
    pub fn new(
        provider: Provider,
        api_key: String,
        base_url: Option<String>,
        default_model: Option<String>,
    ) -> Self {
        let base_url = base_url.unwrap_or_else(|| default_base_url(provider).to_string());
        let default_model = default_model.unwrap_or_else(|| "deepseek-v4-flash".to_string());

        Self {
            client: reqwest::Client::new(),
            provider,
            base_url,
            api_key,
            default_model,
            max_retries: 3,
            base_delay_ms: 1000,
        }
    }

    /// Which provider this client talks to.
    pub fn provider(&self) -> Provider {
        self.provider
    }

    /// Adapt a request to what this client's provider accepts.
    ///
    /// Both `thinking_mode` (DeepSeek) and `reasoning_effort` (Ollama) are
    /// filled in here, from `req.effort`, the harness's own five-level
    /// control. A caller builds a `ChatRequest` by setting `effort` and
    /// leaving both wire fields `None`; this is the one place that maps
    /// `effort` onto whichever field the active provider actually reads.
    /// See `crate::effort::Effort` for the per-provider mapping. A request
    /// with no `effort` set leaves both fields untouched, whatever the
    /// caller put there directly.
    ///
    /// Ollama also does not support `tool_choice`, so that is cleared here
    /// too, regardless of `effort`.
    fn prepare_request(&self, req: &ChatRequest) -> ChatRequest {
        let mut prepared = req.clone();
        match self.provider {
            Provider::DeepSeek => {
                if let Some(effort) = req.effort {
                    prepared.thinking_mode = Some(effort.deepseek_thinking_mode().to_string());
                }
            }
            Provider::Ollama => {
                prepared.tool_choice = None;
                prepared.thinking_mode = None;
                if let Some(effort) = req.effort {
                    prepared.reasoning_effort = Some(effort.ollama_reasoning_effort().to_string());
                }
            }
        }
        prepared
    }

    /// Test-only entry point onto `prepare_request`. `prepare_request` is
    /// called unconditionally by `chat` and `chat_stream`, both of which
    /// make a real HTTP call, so there is no side-effect-free public seam to
    /// drive the wire-mapping tests through directly. The external test
    /// crate's `api_turn.rs` covers the same mapping black-box, over a mock
    /// server, but not every case: the "no effort set, wire field already
    /// carried a value" cases below have no black-box equivalent, since
    /// `AgentConfig` always sets an effort level for a real turn. This
    /// wrapper delegates to the real method in one line and changes nothing
    /// about `prepare_request` itself.
    #[cfg(feature = "test-support")]
    pub fn prepare_request_for_test(&self, req: &ChatRequest) -> ChatRequest {
        self.prepare_request(req)
    }

    /// Send a non-streaming chat completion request (with retry).
    pub async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse> {
        let req = &self.prepare_request(req);
        let url = format!("{}/chat/completions", self.base_url);
        debug!(
            "chat request: model={}, messages={}",
            req.model,
            req.messages.len()
        );

        for attempt in 0..self.max_retries {
            let response = self
                .client
                .post(&url)
                .header(AUTHORIZATION, self.auth_header())
                .header(CONTENT_TYPE, "application/json")
                .json(req)
                .send()
                .await
                .map_err(|e| HarnessError::Api(format!("HTTP request failed: {e}")))?;

            let status = response.status();

            if status.is_success() {
                let chat_response: ChatResponse = response
                    .json()
                    .await
                    .map_err(|e| HarnessError::Parse(format!("Failed to parse response: {e}")))?;

                if let Some(ref usage) = chat_response.usage {
                    info!(
                        "chat response: model={}, tokens in={}, out={}",
                        chat_response.model, usage.prompt_tokens, usage.completion_tokens
                    );
                }

                return Ok(chat_response);
            }

            if !Self::should_retry(status.as_u16()) || attempt + 1 >= self.max_retries {
                let body = response.text().await.unwrap_or_default();
                return Err(HarnessError::Api(format!("API error {status}: {body}")));
            }

            let delay = self.retry_delay(attempt);
            warn!(
                "chat: retry {}/{}, status={}, delay={}ms",
                attempt + 1,
                self.max_retries,
                status.as_u16(),
                delay.as_millis()
            );
            sleep(delay).await;
        }

        unreachable!()
    }

    /// Send a streaming chat completion request (with retry on initial connect).
    ///
    /// Returns an `mpsc::UnboundedReceiver` of parsed `StreamChunk` values.
    pub fn chat_stream(&self, req: &ChatRequest) -> mpsc::UnboundedReceiver<Result<StreamChunk>> {
        let req = &self.prepare_request(req);
        let (tx, rx) = mpsc::unbounded_channel();
        let url = format!("{}/chat/completions", self.base_url);
        let auth = self.auth_header();
        let client = self.client.clone();
        let model = req.model.clone();
        let max_retries = self.max_retries;
        let base_delay_ms = self.base_delay_ms;
        let request_body = match serde_json::to_string(req) {
            Ok(body) => body,
            Err(e) => {
                let _ = tx.send(Err(HarnessError::Parse(format!(
                    "Failed to serialize request: {e}"
                ))));
                return rx;
            }
        };

        tokio::spawn(async move {
            info!("chat stream started: model={}", model);
            debug!("chat stream request body: {}", request_body);

            let response = Self::connect_stream_with_retry(
                &client,
                &url,
                &auth,
                &request_body,
                max_retries,
                base_delay_ms,
            )
            .await;

            let response = match response {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.send(Err(e));
                    return;
                }
            };

            let status = response.status();
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                let _ = tx.send(Err(HarnessError::Api(format!(
                    "API error {status}: {body}"
                ))));
                return;
            }

            let mut stream = response.bytes_stream();
            let mut buffer = String::new();

            while let Some(chunk_result) = stream.next().await {
                let bytes: Bytes = match chunk_result {
                    Ok(b) => b,
                    Err(e) => {
                        let _ = tx.send(Err(HarnessError::Api(format!("Stream error: {e}"))));
                        return;
                    }
                };
                buffer.push_str(&String::from_utf8_lossy(&bytes));

                // SSE lines: "data: {...}\n\n"
                while let Some(pos) = buffer.find("\n\n") {
                    let line = buffer[..pos].to_string();
                    buffer = buffer[pos + 2..].to_string();

                    for sub_line in line.lines() {
                        let trimmed = sub_line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        if let Some(data) = trimmed.strip_prefix("data: ") {
                            if data == "[DONE]" {
                                debug!("stream: received [DONE]");
                                return;
                            }
                            match serde_json::from_str::<StreamChunk>(data) {
                                Ok(chunk) => {
                                    if tx.send(Ok(chunk)).is_err() {
                                        // Receiver dropped
                                        return;
                                    }
                                }
                                Err(e) => {
                                    warn!("stream: failed to parse chunk: {e}");
                                }
                            }
                        }
                    }
                }
            }

            info!("chat stream ended");
        });

        rx
    }

    /// Connect to the streaming endpoint with retry.
    async fn connect_stream_with_retry(
        client: &reqwest::Client,
        url: &str,
        auth: &str,
        body: &str,
        max_retries: u32,
        base_delay_ms: u64,
    ) -> Result<reqwest::Response> {
        for attempt in 0..max_retries {
            match client
                .post(url)
                .header(AUTHORIZATION, auth)
                .header(CONTENT_TYPE, "application/json")
                .body(body.to_string())
                .send()
                .await
            {
                Ok(response) => {
                    let status = response.status();
                    if status.is_success() || !Self::should_retry(status.as_u16()) {
                        return Ok(response);
                    }
                    if attempt + 1 >= max_retries {
                        let body = response.text().await.unwrap_or_default();
                        return Err(HarnessError::Api(format!("API error {status}: {body}")));
                    }
                    let delay_ms = base_delay_ms * 2u64.pow(attempt as u32);
                    warn!(
                        "stream connect: retry {}/{}, status={}, delay={}ms",
                        attempt + 1,
                        max_retries,
                        status.as_u16(),
                        delay_ms
                    );
                    sleep(Duration::from_millis(delay_ms)).await;
                }
                Err(e) => {
                    if attempt + 1 >= max_retries {
                        return Err(HarnessError::Api(format!("HTTP request failed: {e}")));
                    }
                    let delay_ms = base_delay_ms * 2u64.pow(attempt as u32);
                    warn!(
                        "stream connect: retry {}/{}, error={}, delay={}ms",
                        attempt + 1,
                        max_retries,
                        e,
                        delay_ms
                    );
                    sleep(Duration::from_millis(delay_ms)).await;
                }
            }
        }

        unreachable!()
    }

    /// Check whether an HTTP status code warrants a retry.
    fn should_retry(status: u16) -> bool {
        status == 429 || status >= 500
    }

    /// Calculate the retry delay for the given attempt (0-indexed).
    fn retry_delay(&self, attempt: u32) -> Duration {
        Duration::from_millis(self.base_delay_ms * 2u64.pow(attempt))
    }

    /// Build the Bearer auth header value.
    fn auth_header(&self) -> String {
        format!("Bearer {}", self.api_key)
    }
}

/// Default `base_url` for a provider when the caller does not supply one.
fn default_base_url(provider: Provider) -> &'static str {
    match provider {
        Provider::DeepSeek => "https://api.deepseek.com",
        Provider::Ollama => "http://localhost:11434/v1",
    }
}

/// Resolve the API key for `provider` from environment and config files.
///
/// `Provider::Ollama` returns a placeholder immediately. Ollama requires an
/// `Authorization` header to be present but ignores its value entirely. No
/// environment variable or config file is read for it.
///
/// `Provider::DeepSeek` priority: `DEEPSEEK_API_KEY` env var → `settings.json`
/// `api_key` field → error.
pub fn resolve_api_key(provider: Provider, project_root: &std::path::Path) -> Result<String> {
    if provider == Provider::Ollama {
        debug!("api_key: using placeholder value for Ollama");
        return Ok("ollama".to_string());
    }

    // 1. DEEPSEEK_API_KEY env var
    if let Ok(key) = std::env::var("DEEPSEEK_API_KEY") {
        if !key.is_empty() {
            debug!("api_key resolved from DEEPSEEK_API_KEY env var");
            return Ok(key);
        }
    }

    // 2. ANTHROPIC_AUTH_TOKEN env var (set by CustomClaude launcher)
    if let Ok(key) = std::env::var("ANTHROPIC_AUTH_TOKEN") {
        if !key.is_empty() {
            debug!("api_key resolved from ANTHROPIC_AUTH_TOKEN env var");
            return Ok(key);
        }
    }

    // 3. settings.json in project root
    let settings_path = project_root.join("settings.json");
    if let Ok(contents) = std::fs::read_to_string(&settings_path) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&contents) {
            if let Some(key) = json.get("api_key").and_then(|v| v.as_str()) {
                if !key.is_empty() {
                    debug!("api_key resolved from project settings.json");
                    return Ok(key.to_string());
                }
            }
        }
    }

    // 4. ~/.claude/settings.json
    if let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
        let global_settings = std::path::Path::new(&home)
            .join(".claude")
            .join("settings.json");
        if let Ok(contents) = std::fs::read_to_string(&global_settings) {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&contents) {
                if let Some(key) = json.get("api_key").and_then(|v| v.as_str()) {
                    if !key.is_empty() {
                        debug!("api_key resolved from ~/.claude/settings.json");
                        return Ok(key.to_string());
                    }
                }
            }
        }
    }

    // 5. ~/.claude/backends.json (written by the CustomClaude launcher)
    if let Some(key) = key_from_backends_json() {
        debug!("api_key resolved from ~/.claude/backends.json");
        return Ok(key);
    }

    Err(HarnessError::Config(
        "DEEPSEEK_API_KEY not set. Get one at https://platform.deepseek.com/api_keys".to_string(),
    ))
}

/// Read `~/.claude/backends.json` and pull the DeepSeek key out of it.
pub fn key_from_backends_json() -> Option<String> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()?;
    let path = std::path::Path::new(&home)
        .join(".claude")
        .join("backends.json");
    let contents = std::fs::read_to_string(path).ok()?;
    parse_backends_json(&contents)
}

/// Pick an `apiKey` out of the backends config.
///
/// The backend named by `default` wins. Otherwise the first backend that
/// carries a key wins, so a config with a single DeepSeek entry still works.
pub fn parse_backends_json(contents: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(contents).ok()?;
    let backends = json.get("backends")?.as_object()?;

    let default_key = json
        .get("default")
        .and_then(|v| v.as_str())
        .and_then(|name| backends.get(name))
        .and_then(backend_api_key);
    if default_key.is_some() {
        return default_key;
    }

    backends.values().find_map(backend_api_key)
}

/// Read the non-empty `apiKey` field of one backend entry.
fn backend_api_key(backend: &serde_json::Value) -> Option<String> {
    let key = backend.get("apiKey")?.as_str()?;
    if key.is_empty() {
        return None;
    }
    Some(key.to_string())
}
