//! Types for the inner `event` object carried by a `"type":"stream_event"` line.
//!
//! These mirror the Anthropic streaming content-block protocol. Every field
//! uses `#[serde(default)]` generously so an event Claude Code adds or omits
//! a field for never breaks the parse.

use serde::Deserialize;

/// Token usage counters. All fields default to zero, since not every usage
/// object in the protocol carries every field.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
}

/// The envelope for `{"type":"stream_event","event":{...},...}`.
#[derive(Debug, Clone, Deserialize)]
pub struct StreamEventEnvelope {
    pub event: InnerStreamEvent,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub parent_tool_use_id: Option<String>,
    #[serde(default)]
    pub uuid: Option<String>,
}

/// The inner `event` object, discriminated by its own `"type"` field.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum InnerStreamEvent {
    #[serde(rename = "message_start")]
    MessageStart { message: StreamMessageStart },
    #[serde(rename = "content_block_start")]
    ContentBlockStart {
        index: u32,
        content_block: ContentBlock,
    },
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta { index: u32, delta: ContentDelta },
    #[serde(rename = "content_block_stop")]
    ContentBlockStop { index: u32 },
    #[serde(rename = "message_delta")]
    MessageDelta {
        delta: MessageDeltaInfo,
        #[serde(default)]
        usage: Option<Usage>,
    },
    #[serde(rename = "message_stop")]
    MessageStop,
    #[serde(other)]
    Unknown,
}

/// The `message` object inside `message_start`.
#[derive(Debug, Clone, Deserialize)]
pub struct StreamMessageStart {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

/// A content block, as carried by `content_block_start` and by the completed
/// `content` array of an `"assistant"` event.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text {
        #[serde(default)]
        text: String,
    },
    #[serde(rename = "thinking")]
    Thinking {
        #[serde(default)]
        thinking: String,
        #[serde(default)]
        signature: String,
    },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: serde_json::Value,
    },
    #[serde(other)]
    Unknown,
}

/// A `content_block_delta`'s `delta` object.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum ContentDelta {
    #[serde(rename = "text_delta")]
    TextDelta {
        #[serde(default)]
        text: String,
    },
    #[serde(rename = "thinking_delta")]
    ThinkingDelta {
        #[serde(default)]
        thinking: String,
    },
    #[serde(rename = "signature_delta")]
    SignatureDelta {
        #[serde(default)]
        signature: String,
    },
    #[serde(rename = "input_json_delta")]
    InputJsonDelta {
        #[serde(default)]
        partial_json: String,
    },
    #[serde(other)]
    Unknown,
}

/// The `delta` object inside `message_delta`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MessageDeltaInfo {
    #[serde(default)]
    pub stop_reason: Option<String>,
}
