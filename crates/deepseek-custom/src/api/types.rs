use serde::{Deserialize, Serialize};

use crate::effort::Effort;

// ── Request types ──

#[derive(Debug, Clone, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDef>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Legacy format for deepseek-chat: `{"type": "enabled"}`. Not used for V4 models.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingConfig>,
    /// DeepSeek V4 format: `"thinking"`, `"non-thinking"`, or `"thinking_max"`.
    /// Filled in by `ApiClient::prepare_request` from `effort` below; a
    /// caller building a `ChatRequest` should leave this `None` and set
    /// `effort` instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_mode: Option<String>,
    /// Ollama's `/v1/chat/completions` thinking control: `"high" | "medium" |
    /// "low" | "max" | "none"`. Unused by DeepSeek. Filled in by
    /// `ApiClient::prepare_request` from `effort` below, the same way as
    /// `thinking_mode`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// The harness's own five-level effort control. Never sent on the wire:
    /// `ApiClient::prepare_request` reads it to fill in `thinking_mode`
    /// (DeepSeek) or `reasoning_effort` (Ollama) per provider, at the edge,
    /// right before the request goes out. See `crate::effort::Effort`.
    #[serde(skip)]
    pub effort: Option<Effort>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Content>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

/// A message's content, either a plain string or a list of parts.
///
/// `#[serde(untagged)]` is what keeps the wire shape unchanged for every
/// text-only message: `Text(String)` serializes as a bare JSON string, the
/// exact shape the old `Option<String>` field produced, and `Parts(Vec<
/// ContentPart>)` serializes as a JSON array, the OpenAI-compatible shape
/// both API providers speak for a message that mixes text and image parts.
/// Deserializing tries `Text` first, so a plain string on the wire (every
/// message this harness has ever sent) still parses as `Text`, including a
/// session file saved before this type existed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Content {
    Text(String),
    Parts(Vec<ContentPart>),
}

impl Content {
    /// Build a `Text` variant from anything that converts to a `String`.
    pub fn text(text: impl Into<String>) -> Self {
        Content::Text(text.into())
    }

    /// The plain text, if this is a `Text` variant. `None` for `Parts`:
    /// there is no single string to hand back once content has more than
    /// one part.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Content::Text(s) => Some(s),
            Content::Parts(_) => None,
        }
    }
}

/// One part of a multi-part message. `Text` carries plain text. `ImageUrl`
/// carries a `data:` URL with a base64-encoded image payload.
///
/// Serializes to the OpenAI-compatible wire shape both API providers speak:
/// `{"type":"text","text":"..."}` or `{"type":"image_url","image_url":
/// {"url":"..."}}`. The Rust-side `ImageUrl` variant keeps `url` as a flat
/// field rather than mirroring that nested wire shape, so `Serialize` and
/// `Deserialize` are hand-written here instead of derived.
#[derive(Debug, Clone, PartialEq)]
pub enum ContentPart {
    Text { text: String },
    ImageUrl { url: String },
}

impl Serialize for ContentPart {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        match self {
            ContentPart::Text { text } => {
                let mut s = serializer.serialize_struct("ContentPart", 2)?;
                s.serialize_field("type", "text")?;
                s.serialize_field("text", text)?;
                s.end()
            }
            ContentPart::ImageUrl { url } => {
                let mut s = serializer.serialize_struct("ContentPart", 2)?;
                s.serialize_field("type", "image_url")?;
                s.serialize_field("image_url", &ImageUrlPayload { url: url.clone() })?;
                s.end()
            }
        }
    }
}

/// The nested `{"url": "..."}` object an `image_url` content part wraps its
/// URL in on the wire. Exists only to give `ContentPart`'s hand-written
/// `Serialize`/`Deserialize` a typed shape for that nesting; nothing else
/// in the crate refers to it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ImageUrlPayload {
    url: String,
}

/// One outgoing image attachment on a user turn, before it has been mapped
/// onto any particular backend's wire shape. `data` is the raw base64
/// payload with no `data:` prefix. `media_type` is the image's MIME type,
/// e.g. `"image/png"`.
///
/// Backend-agnostic on purpose: `src/agent/agent_loop.rs`'s
/// `build_user_content` maps this onto the OpenAI `image_url` part for
/// Ollama, or drops it with a transcript notice for DeepSeek.
/// `src/backend/claude_cli/process.rs`'s `build_user_turn_line` maps it
/// onto the Anthropic `image` content block instead. Each backend needs its
/// own shape; see `docs/notes/image-support.md` for what was confirmed
/// against each one.
///
/// `Serialize`/`Deserialize` are derived so this type can sit inside
/// `BlockKind::Image` in `src/gui/transcript.rs` and round-trip through a
/// session file. The base64 `data` field is what actually goes to disk in
/// that case: a screenshot-sized payload is a real cost per saved session,
/// noted where the `Image` block is defined.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageAttachment {
    pub data: String,
    pub media_type: String,
}

impl<'de> Deserialize<'de> for ContentPart {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Raw {
            #[serde(rename = "type")]
            kind: String,
            #[serde(default)]
            text: Option<String>,
            #[serde(default)]
            image_url: Option<ImageUrlPayload>,
        }

        let raw = Raw::deserialize(deserializer)?;
        match raw.kind.as_str() {
            "text" => Ok(ContentPart::Text {
                text: raw.text.unwrap_or_default(),
            }),
            "image_url" => Ok(ContentPart::ImageUrl {
                url: raw.image_url.map(|i| i.url).unwrap_or_default(),
            }),
            other => Err(serde::de::Error::custom(format!(
                "unknown content part type: {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    #[serde(default)]
    pub id: String,
    #[serde(rename = "type")]
    #[serde(default)]
    pub call_type: String,
    #[serde(default)]
    pub function: Option<FunctionCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FunctionCall {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolDef {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: FunctionDef,
}

#[derive(Debug, Clone, Serialize)]
pub struct FunctionDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolChoice {
    None,
    Auto,
    Required,
}

#[derive(Debug, Clone, Serialize)]
pub struct ThinkingConfig {
    #[serde(rename = "type")]
    pub thinking_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

// ── Response types ──

#[derive(Debug, Clone, Deserialize)]
pub struct ChatResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<Choice>,
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Choice {
    pub index: u32,
    pub message: Message,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    /// Tokens retrieved from prompt cache (not charged as input).
    #[serde(default)]
    pub prompt_cache_hit_tokens: u32,
    /// Tokens not found in prompt cache (charged as input).
    #[serde(default)]
    pub prompt_cache_miss_tokens: u32,
    /// Tokens written to prompt cache for future hits.
    #[serde(default)]
    pub prompt_cache_write_tokens: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StreamChunk {
    pub id: Option<String>,
    pub object: Option<String>,
    pub created: Option<u64>,
    pub model: Option<String>,
    pub choices: Option<Vec<StreamChoice>>,
    /// Usage stats sent in the final streaming chunk (before [DONE]).
    #[serde(default)]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StreamChoice {
    pub index: u32,
    pub delta: Delta,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Delta {
    #[serde(default)]
    pub role: Option<Role>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(default)]
    pub reasoning_content: Option<String>,
}

// ── Tool result (for feeding back into agent loop) ──

#[derive(Debug, Clone, Serialize)]
pub struct ToolResult {
    pub tool_call_id: String,
    pub role: String,
    pub content: String,
}

