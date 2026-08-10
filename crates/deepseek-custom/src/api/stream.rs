use bytes::Bytes;
use futures::StreamExt;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::sleep;
use tracing::{debug, info, warn};

use crate::api::client::ApiClient;
use crate::api::types::*;
use crate::error::{HarnessError, Result};

impl ApiClient {
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

            let response = connect_stream_with_retry(
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

            read_sse_stream(response, &tx).await;
            info!("chat stream ended");
        });

        rx
    }
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
                if status.is_success() || !ApiClient::should_retry(status.as_u16()) {
                    return Ok(response);
                }
                if attempt + 1 >= max_retries {
                    let body = response.text().await.unwrap_or_default();
                    return Err(HarnessError::Api(format!("API error {status}: {body}")));
                }
                let delay_ms = base_delay_ms * 2u64.pow(attempt);
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
                let delay_ms = base_delay_ms * 2u64.pow(attempt);
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

/// Read the SSE byte stream from `response` and push parsed chunks into `tx`.
async fn read_sse_stream(
    response: reqwest::Response,
    tx: &mpsc::UnboundedSender<Result<StreamChunk>>,
) {
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
            if !process_sse_line(&line, tx) {
                return;
            }
        }
    }
}

/// Process one SSE frame (one logical line between `\n\n` delimiters).
///
/// Returns `false` when `[DONE]` is received or the receiver has been
/// dropped, signalling the caller to stop reading.
fn process_sse_line(line: &str, tx: &mpsc::UnboundedSender<Result<StreamChunk>>) -> bool {
    for sub_line in line.lines() {
        let trimmed = sub_line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(data) = trimmed.strip_prefix("data: ") {
            if data == "[DONE]" {
                debug!("stream: received [DONE]");
                return false;
            }
            match serde_json::from_str::<StreamChunk>(data) {
                Ok(chunk) => {
                    if tx.send(Ok(chunk)).is_err() {
                        return false;
                    }
                }
                Err(e) => {
                    warn!("stream: failed to parse chunk: {e}");
                }
            }
        }
    }
    true
}
