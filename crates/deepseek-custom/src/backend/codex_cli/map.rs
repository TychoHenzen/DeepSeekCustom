//! Maps `codex exec --json` events onto the stream events used by the GUI.
//!
//! Codex item updates are lifecycle snapshots rather than text deltas. This
//! mapper remembers each item's prior snapshot and emits only newly appended
//! content, so repeated snapshots cannot duplicate assistant output.

use std::collections::HashMap;

use crate::agent::events::StreamEvent;

use super::events::{CodexEvent, CodexItem, CodexUsage};

/// Converts Codex JSONL events into the stream events used by the harness.
pub struct EventMapper {
    turn: u32,
    item_snapshots: HashMap<String, String>,
}

impl EventMapper {
    pub fn new() -> Self {
        Self {
            turn: 0,
            item_snapshots: HashMap::new(),
        }
    }

    /// Map one protocol event to zero or more stream events.
    pub fn map(&mut self, event: CodexEvent) -> Vec<StreamEvent> {
        match event {
            CodexEvent::ThreadStarted(_) | CodexEvent::TurnStarted(_) => Vec::new(),
            CodexEvent::ItemStarted(event) => self.map_item_started(event.item),
            CodexEvent::ItemUpdated(event) => self.map_item_updated(event.item),
            CodexEvent::ItemCompleted(event) => self.map_item_completed(event.item),
            CodexEvent::TurnCompleted(event) => self.map_turn_completed(event.usage),
            CodexEvent::TurnFailed(event) => {
                self.turn += 1;
                self.item_snapshots.clear();
                vec![StreamEvent::Error {
                    message: event.error.message,
                }]
            }
        }
    }

    fn map_item_started(&mut self, item: CodexItem) -> Vec<StreamEvent> {
        let (tool, args) = match item.item_type.as_str() {
            "command_execution" => (
                "command_execution",
                json_object("command", item.command.map(serde_json::Value::String)),
            ),
            "file_change" => ("file_change", json_object("changes", item.changes)),
            "mcp_tool_call" => (
                "mcp_tool_call",
                mcp_arguments(item.server, item.tool, item.arguments),
            ),
            "agent_message" | "reasoning" => return self.map_item_snapshot(item),
            other => {
                tracing::warn!("codex_cli: ignoring unknown item kind {other:?}");
                return Vec::new();
            }
        };

        vec![StreamEvent::ToolCallStart {
            turn: self.turn,
            tool: tool.to_string(),
            args,
        }]
    }

    fn map_item_completed(&mut self, item: CodexItem) -> Vec<StreamEvent> {
        match item.item_type.as_str() {
            "agent_message" | "reasoning" => self.map_item_snapshot(item),
            "command_execution" => vec![StreamEvent::ToolCallEnd {
                turn: self.turn,
                tool: "command_execution".to_string(),
                output: item.aggregated_output.unwrap_or_default(),
                is_error: item.exit_code.is_some_and(|code| code != 0) || item.error.is_some(),
            }],
            "file_change" => vec![StreamEvent::ToolCallEnd {
                turn: self.turn,
                tool: "file_change".to_string(),
                output: value_or_status(item.result.or(item.changes), item.status),
                is_error: item.error.is_some(),
            }],
            "mcp_tool_call" => vec![StreamEvent::ToolCallEnd {
                turn: self.turn,
                tool: "mcp_tool_call".to_string(),
                output: value_or_status(item.result.or(item.error.clone()), item.status),
                is_error: item.error.is_some(),
            }],
            other => {
                tracing::warn!("codex_cli: ignoring unknown item kind {other:?}");
                Vec::new()
            }
        }
    }

    fn map_item_updated(&mut self, item: CodexItem) -> Vec<StreamEvent> {
        match item.item_type.as_str() {
            "agent_message" | "reasoning" => self.map_item_snapshot(item),
            "command_execution" | "file_change" | "mcp_tool_call" => Vec::new(),
            other => {
                tracing::warn!("codex_cli: ignoring unknown item kind {other:?}");
                Vec::new()
            }
        }
    }

    fn map_item_snapshot(&mut self, item: CodexItem) -> Vec<StreamEvent> {
        let reasoning = match item.item_type.as_str() {
            "agent_message" => false,
            "reasoning" => true,
            _ => return Vec::new(),
        };
        let text = item.text.unwrap_or_default();
        if text.is_empty() {
            return Vec::new();
        }

        let key = item
            .id
            .map(|id| format!("{}:{id}", item.item_type))
            .unwrap_or_else(|| item.item_type.clone());
        let delta = match self.item_snapshots.get(&key) {
            None => text.clone(),
            Some(previous) if previous == &text => return Vec::new(),
            Some(previous) if text.starts_with(previous) => text[previous.len()..].to_string(),
            Some(previous) => {
                tracing::warn!(
                    "codex_cli: {key:?} snapshot replaced non-prefix content ({} bytes with {} bytes)",
                    previous.len(),
                    text.len()
                );
                text.clone()
            }
        };
        self.item_snapshots.insert(key, text);

        if reasoning {
            vec![StreamEvent::Reasoning {
                turn: self.turn,
                text: delta,
            }]
        } else {
            vec![StreamEvent::Text {
                turn: self.turn,
                text: delta,
            }]
        }
    }

    fn map_turn_completed(&mut self, usage: Option<CodexUsage>) -> Vec<StreamEvent> {
        let usage = usage.unwrap_or_default();
        let turn = self.turn;
        self.turn += 1;
        self.item_snapshots.clear();

        vec![StreamEvent::TurnEnd {
            turn,
            finish_reason: "end_turn".to_string(),
            total_tokens: saturating_usize(usage.input_tokens.saturating_add(usage.output_tokens)),
            prompt_cache_hit_tokens: saturating_u32(usage.cached_input_tokens),
            prompt_cache_miss_tokens: saturating_u32(
                usage.input_tokens.saturating_sub(usage.cached_input_tokens),
            ),
        }]
    }
}

impl Default for EventMapper {
    fn default() -> Self {
        Self::new()
    }
}

fn json_object(name: &str, value: Option<serde_json::Value>) -> String {
    let mut object = serde_json::Map::new();
    object.insert(name.to_string(), value.unwrap_or(serde_json::Value::Null));
    serde_json::Value::Object(object).to_string()
}

fn mcp_arguments(
    server: Option<String>,
    tool: Option<String>,
    arguments: Option<serde_json::Value>,
) -> String {
    let mut object = serde_json::Map::new();
    object.insert("server".to_string(), server.into());
    object.insert("tool".to_string(), tool.into());
    object.insert(
        "arguments".to_string(),
        arguments.unwrap_or(serde_json::Value::Null),
    );
    serde_json::Value::Object(object).to_string()
}

fn value_or_status(value: Option<serde_json::Value>, status: Option<String>) -> String {
    value
        .map(|value| match value {
            serde_json::Value::String(text) => text,
            value => value.to_string(),
        })
        .or(status)
        .unwrap_or_default()
}

fn saturating_u32(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn saturating_usize(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}
