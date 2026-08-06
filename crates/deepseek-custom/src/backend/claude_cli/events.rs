//! Line protocol for `claude -p --output-format stream-json`.
//!
//! Every event is one JSON object on one line, discriminated by a top-level
//! `"type"` field, and for `"system"` further by a `"subtype"` field. This
//! module only parses the protocol. It has no dependency on the agent, GUI,
//! or process modules.

use serde::Deserialize;

use super::stream::{StreamEventEnvelope, Usage};

/// One parsed line of the stream-json protocol.
#[derive(Debug, Clone)]
pub enum ClaudeEvent {
    System(SystemEvent),
    RateLimitEvent(RateLimitEventData),
    StreamEvent(StreamEventEnvelope),
    Assistant(AssistantEventData),
    User(UserEventData),
    Result(ResultEventData),
    /// A `"type"` value this parser does not recognize. Carries the raw
    /// type string so a newly added event kind is ignored, not fatal.
    Unknown(String),
}

/// Parse one line of the stream-json protocol.
///
/// Returns `None` for a blank line, a line that is not JSON, or a line that
/// does not start with `{` after trimming. Never panics, never returns an
/// error: a malformed line is skipped so one bad line cannot kill a live
/// session.
pub fn parse_line(line: &str) -> Option<ClaudeEvent> {
    let trimmed = line.trim();
    if trimmed.is_empty() || !trimmed.starts_with('{') {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let type_str = value.get("type")?.as_str()?;

    match type_str {
        "system" => parse_system(&value).map(ClaudeEvent::System),
        "rate_limit_event" => serde_json::from_value(value)
            .ok()
            .map(ClaudeEvent::RateLimitEvent),
        "stream_event" => serde_json::from_value(value)
            .ok()
            .map(ClaudeEvent::StreamEvent),
        "assistant" => serde_json::from_value(value)
            .ok()
            .map(ClaudeEvent::Assistant),
        "user" => serde_json::from_value(value).ok().map(ClaudeEvent::User),
        "result" => serde_json::from_value(value)
            .ok()
            .map(ClaudeEvent::Result),
        other => Some(ClaudeEvent::Unknown(other.to_string())),
    }
}

fn parse_system(value: &serde_json::Value) -> Option<SystemEvent> {
    let subtype = value.get("subtype")?.as_str()?;
    match subtype {
        "init" => serde_json::from_value(value.clone()).ok().map(SystemEvent::Init),
        "status" => serde_json::from_value(value.clone())
            .ok()
            .map(SystemEvent::Status),
        "thinking_tokens" => serde_json::from_value(value.clone())
            .ok()
            .map(SystemEvent::ThinkingTokens),
        "hook_started" => serde_json::from_value(value.clone())
            .ok()
            .map(SystemEvent::HookStarted),
        "hook_response" => serde_json::from_value(value.clone())
            .ok()
            .map(SystemEvent::HookResponse),
        other => Some(SystemEvent::Unknown(other.to_string())),
    }
}

/// A `"type":"system"` event, dispatched further by its `"subtype"` field.
#[derive(Debug, Clone)]
pub enum SystemEvent {
    Init(SystemInit),
    Status(SystemStatus),
    ThinkingTokens(SystemThinkingTokens),
    HookStarted(SystemHookStarted),
    HookResponse(SystemHookResponse),
    /// A `"subtype"` value this parser does not recognize.
    Unknown(String),
}

#[derive(Debug, Clone, Deserialize)]
pub struct SystemInit {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerInfo>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpServerInfo {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SystemStatus {
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SystemThinkingTokens {
    #[serde(default)]
    pub estimated_tokens: i64,
    #[serde(default)]
    pub estimated_tokens_delta: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SystemHookStarted {
    #[serde(default)]
    pub hook_name: Option<String>,
    #[serde(default)]
    pub hook_event: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SystemHookResponse {
    #[serde(default)]
    pub hook_name: Option<String>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub outcome: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RateLimitEventData {
    #[serde(default)]
    pub rate_limit_info: Option<serde_json::Value>,
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssistantEventData {
    pub message: AssistantMessage,
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssistantMessage {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub content: Vec<super::stream::ContentBlock>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UserEventData {
    pub message: UserMessage,
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UserMessage {
    #[serde(default)]
    pub content: Vec<ToolResultBlock>,
}

/// A `tool_result` content block. Its `content` field may be a plain string
/// or an array of blocks, so it is kept as a raw `serde_json::Value`. Use
/// [`ToolResultBlock::content_as_string`] to render it.
#[derive(Debug, Clone, Deserialize)]
pub struct ToolResultBlock {
    pub tool_use_id: String,
    #[serde(default)]
    pub content: serde_json::Value,
    #[serde(default)]
    pub is_error: bool,
}

impl ToolResultBlock {
    /// Render `content` to a plain string, whether it arrived as a string
    /// or as an array of blocks.
    pub fn content_as_string(&self) -> String {
        match &self.content {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResultEventData {
    #[serde(default)]
    pub subtype: Option<String>,
    #[serde(default)]
    pub is_error: bool,
    #[serde(default)]
    pub duration_api_ms: Option<u64>,
    #[serde(default)]
    pub num_turns: Option<u64>,
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub total_cost_usd: Option<f64>,
    #[serde(default)]
    pub usage: Option<Usage>,
    #[serde(default)]
    pub result: Option<String>,
}
