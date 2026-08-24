use std::time::Duration;

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use tokio::time::sleep;
use tracing::{debug, info, warn};

use crate::api::provider::Provider;
use crate::api::types::*;
use crate::error::{HarnessError, Result};

/// Client for an OpenAI-compatible chat completions API (DeepSeek or Ollama).
pub struct ApiClient {
    pub(crate) client: reqwest::Client,
    provider: Provider,
    pub(crate) base_url: String,
    api_key: String,
    pub(crate) max_retries: u32,
    pub(crate) base_delay_ms: u64,
}

impl ApiClient {
    /// Create a new ApiClient.
    ///
    /// `api_key` is required. `base_url` defaults per provider when `None`:
    /// `https://api.deepseek.com` for `Provider::DeepSeek`, and
    /// `http://localhost:11434/v1` for `Provider::Ollama`.
    ///
    /// There is no default model here. Every `ChatRequest` carries its own
    /// `model`, filled from the shared `model_flag` each turn, so a default
    /// held on the client would never be consulted.
    pub fn new(provider: Provider, api_key: String, base_url: Option<String>) -> Self {
        let base_url = base_url.unwrap_or_else(|| default_base_url(provider).to_string());

        Self {
            client: reqwest::Client::new(),
            provider,
            base_url,
            api_key,
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
    pub(crate) fn prepare_request(&self, req: &ChatRequest) -> ChatRequest {
        let mut prepared = req.clone();
        match self.provider {
            Provider::DeepSeek => {
                prepared.response_format = None;
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

            let exhausted = !Self::should_retry(status.as_u16()) || attempt + 1 >= self.max_retries;
            if exhausted {
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

    /// Check whether an HTTP status code warrants a retry.
    pub(crate) fn should_retry(status: u16) -> bool {
        status == 429 || status >= 500
    }

    /// Calculate the retry delay for the given attempt (0-indexed).
    fn retry_delay(&self, attempt: u32) -> Duration {
        Duration::from_millis(self.base_delay_ms * 2u64.pow(attempt))
    }

    /// Build the Bearer auth header value.
    pub(crate) fn auth_header(&self) -> String {
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
