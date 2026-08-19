//! Line protocol for `codex exec --json`.
//!
//! Every event is one JSON object on one line, discriminated by a top-level
//! `"type"` field. This module only parses and retains protocol data.

use std::collections::HashMap;

use serde::Deserialize;

/// One parsed line of the Codex JSONL protocol.
#[derive(Debug, Clone)]
pub enum CodexEvent {
    ThreadStarted(ThreadStarted),
    TurnStarted(TurnStarted),
    ItemStarted(ItemStarted),
    ItemUpdated(ItemUpdated),
    ItemCompleted(ItemCompleted),
    TurnCompleted(TurnCompleted),
    TurnFailed(TurnFailed),
}

/// Parse one line of the Codex JSONL protocol.
///
/// Malformed lines and unknown event types are logged and skipped. A bad line
/// therefore cannot prevent the caller from parsing later lines.
pub fn parse_event(line: &str) -> Option<CodexEvent> {
    let value: serde_json::Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!("codex_cli: skipping malformed JSONL event: {error}");
            return None;
        }
    };

    let Some(event_type) = value.get("type").and_then(serde_json::Value::as_str) else {
        tracing::warn!("codex_cli: skipping JSONL event without a string type field");
        return None;
    };

    let parsed = match event_type {
        "thread.started" => parse_as(value, CodexEvent::ThreadStarted),
        "turn.started" => parse_as(value, CodexEvent::TurnStarted),
        "item.started" => parse_as(value, CodexEvent::ItemStarted),
        "item.updated" => parse_as(value, CodexEvent::ItemUpdated),
        "item.completed" => parse_as(value, CodexEvent::ItemCompleted),
        "turn.completed" => parse_as(value, CodexEvent::TurnCompleted),
        "turn.failed" => parse_as(value, CodexEvent::TurnFailed),
        other => {
            tracing::warn!("codex_cli: skipping unknown JSONL event type {other:?}");
            return None;
        }
    };

    match parsed {
        Ok(event) => Some(event),
        Err(error) => {
            tracing::warn!("codex_cli: skipping invalid {event_type:?} event: {error}");
            None
        }
    }
}

fn parse_as<T>(
    value: serde_json::Value,
    wrap: impl FnOnce(T) -> CodexEvent,
) -> serde_json::Result<CodexEvent>
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_value(value).map(wrap)
}

#[derive(Debug, Clone, Deserialize)]
pub struct ThreadStarted {
    pub thread_id: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct TurnStarted {}

#[derive(Debug, Clone, Deserialize)]
pub struct ItemStarted {
    pub item: CodexItem,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ItemUpdated {
    pub item: CodexItem,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ItemCompleted {
    pub item: CodexItem,
}

/// Item payload shared by item lifecycle events.
///
/// Codex item kinds do not share one fixed schema. Named fields cover the
/// item kinds mapped by this harness. `extra` retains fields added by Codex.
#[derive(Debug, Clone, Deserialize)]
pub struct CodexItem {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub item_type: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub aggregated_output: Option<String>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub changes: Option<serde_json::Value>,
    #[serde(default)]
    pub server: Option<String>,
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub arguments: Option<serde_json::Value>,
    #[serde(default)]
    pub result: Option<serde_json::Value>,
    #[serde(default)]
    pub error: Option<serde_json::Value>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct TurnCompleted {
    #[serde(default)]
    pub usage: Option<CodexUsage>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct CodexUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub cached_input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TurnFailed {
    pub error: CodexError,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CodexError {
    pub message: String,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}
