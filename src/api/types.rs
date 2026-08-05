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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_request_serializes_correctly() {
        let req = ChatRequest {
            model: "deepseek-v4-flash".into(),
            messages: vec![Message {
                role: Role::User,
                content: Some(Content::text("hello")),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            }],
            tools: None,
            tool_choice: None,
            stream: false,
            temperature: Some(0.7),
            max_tokens: Some(1024),
            thinking: None,
            thinking_mode: None,
            reasoning_effort: None,
            effort: None,
        };

        let json = serde_json::to_string(&req).expect("serialize");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");

        assert_eq!(parsed["model"], "deepseek-v4-flash");
        assert_eq!(parsed["messages"][0]["role"], "user");
        assert_eq!(parsed["messages"][0]["content"], "hello");
        assert_eq!(parsed["stream"], false);
        assert_eq!(parsed["temperature"], 0.7);
        assert_eq!(parsed["max_tokens"], 1024);
        assert!(parsed.get("thinking").is_none());
        assert!(parsed.get("thinking_mode").is_none());
    }

    #[test]
    fn thinking_enabled_serializes_correctly() {
        let req = ChatRequest {
            model: "deepseek-v4-flash".into(),
            messages: vec![Message {
                role: Role::User,
                content: Some(Content::text("hello")),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            }],
            tools: None,
            tool_choice: None,
            stream: true,
            temperature: Some(0.7),
            max_tokens: Some(4096),
            thinking: None,
            thinking_mode: Some("thinking".into()),
            reasoning_effort: None,
            effort: None,
        };

        let json = serde_json::to_string(&req).expect("serialize");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");

        assert_eq!(parsed["thinking_mode"], "thinking");
        assert!(parsed.get("thinking").is_none());
    }

    #[test]
    fn thinking_disabled_serializes_correctly() {
        let req = ChatRequest {
            model: "deepseek-v4-flash".into(),
            messages: vec![Message {
                role: Role::User,
                content: Some(Content::text("hello")),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            }],
            tools: None,
            tool_choice: None,
            stream: true,
            temperature: Some(0.7),
            max_tokens: Some(4096),
            thinking: None,
            thinking_mode: Some("non-thinking".into()),
            reasoning_effort: None,
            effort: None,
        };

        let json = serde_json::to_string(&req).expect("serialize");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");

        assert_eq!(parsed["thinking_mode"], "non-thinking");
        assert!(parsed.get("thinking").is_none());
    }

    #[test]
    fn reasoning_effort_is_absent_when_none() {
        let req = ChatRequest {
            model: "deepseek-v4-flash".into(),
            messages: vec![Message {
                role: Role::User,
                content: Some(Content::text("hello")),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            }],
            tools: None,
            tool_choice: None,
            stream: false,
            temperature: Some(0.7),
            max_tokens: Some(1024),
            thinking: None,
            thinking_mode: None,
            reasoning_effort: None,
            effort: None,
        };

        let json = serde_json::to_string(&req).expect("serialize");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");

        assert!(parsed.get("reasoning_effort").is_none());
    }

    #[test]
    fn effort_field_never_appears_on_the_wire_even_when_set() {
        let req = ChatRequest {
            model: "deepseek-v4-flash".into(),
            messages: vec![Message {
                role: Role::User,
                content: Some(Content::text("hello")),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            }],
            tools: None,
            tool_choice: None,
            stream: false,
            temperature: Some(0.7),
            max_tokens: Some(1024),
            thinking: None,
            thinking_mode: None,
            reasoning_effort: None,
            effort: Some(crate::effort::Effort::Max),
        };

        let json = serde_json::to_string(&req).expect("serialize");
        assert!(!json.contains("effort"));
    }

    #[test]
    fn stream_chunk_parses_reasoning_content() {
        let data = r#"{"id":"chatcmpl-xyz","object":"chat.completion.chunk","created":1716902400,"model":"deepseek-v4-flash","choices":[{"index":0,"delta":{"content":null,"reasoning_content":"First I will think about this problem carefully"},"finish_reason":null}]}"#;
        let chunk: StreamChunk = serde_json::from_str(data).expect("parse");

        let choices = chunk.choices.as_ref().unwrap();
        assert_eq!(
            choices[0].delta.reasoning_content.as_deref(),
            Some("First I will think about this problem carefully")
        );
        assert!(choices[0].delta.content.is_none());
    }

    #[test]
    fn chat_response_deserializes_from_fixture() {
        let json = include_str!("../../tests/fixtures/chat_response.json");
        let response: ChatResponse = serde_json::from_str(json).expect("deserialize");

        assert_eq!(response.id, "chatcmpl-abc123");
        assert_eq!(response.model, "deepseek-v4-flash");
        assert_eq!(response.choices.len(), 1);
        assert_eq!(response.choices[0].finish_reason.as_deref(), Some("stop"));
        assert_eq!(
            response.choices[0].message.content.as_ref().and_then(Content::as_text),
            Some("Hello! I'm DeepSeek, an AI assistant. How can I help you today?")
        );
        let usage = response.usage.as_ref().unwrap();
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 15);
        assert_eq!(usage.total_tokens, 25);
        assert_eq!(usage.prompt_cache_hit_tokens, 8);
        assert_eq!(usage.prompt_cache_miss_tokens, 2);
        assert_eq!(usage.prompt_cache_write_tokens, 10);
    }

    #[test]
    fn stream_chunk_parses_sse_data_line() {
        let data = r#"{"id":"chatcmpl-xyz","object":"chat.completion.chunk","created":1716902400,"model":"deepseek-v4-flash","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}"#;
        let chunk: StreamChunk = serde_json::from_str(data).expect("parse");

        let choices = chunk.choices.as_ref().unwrap();
        assert_eq!(choices[0].delta.content.as_deref(), Some("Hello"));
    }

    #[test]
    fn stream_chunk_parses_done_marker() {
        let data = r#"{"id":"chatcmpl-xyz","object":"chat.completion.chunk","created":1716902400,"model":"deepseek-v4-flash","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#;
        let chunk: StreamChunk = serde_json::from_str(data).expect("parse");

        let choices = chunk.choices.as_ref().unwrap();
        assert_eq!(choices[0].finish_reason.as_deref(), Some("stop"));
        assert!(choices[0].delta.content.is_none());
    }

    #[test]
    fn stream_chunk_parses_final_chunk_with_usage() {
        // Final chunk with usage (DeepSeek sends cache stats in last delta before [DONE])
        let data = r#"{"id":"chatcmpl-xyz","object":"chat.completion.chunk","created":1716902400,"model":"deepseek-v4-flash","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":120,"completion_tokens":45,"total_tokens":165,"prompt_cache_hit_tokens":100,"prompt_cache_miss_tokens":20,"prompt_cache_write_tokens":120}}"#;
        let chunk: StreamChunk = serde_json::from_str(data).expect("parse");

        let usage = chunk.usage.as_ref().unwrap();
        assert_eq!(usage.prompt_tokens, 120);
        assert_eq!(usage.completion_tokens, 45);
        assert_eq!(usage.total_tokens, 165);
        assert_eq!(usage.prompt_cache_hit_tokens, 100);
        assert_eq!(usage.prompt_cache_miss_tokens, 20);
        assert_eq!(usage.prompt_cache_write_tokens, 120);
    }

    #[test]
    fn usage_without_cache_fields_defaults_to_zero() {
        // Backward compat: older API responses or non-cached requests don't include cache fields
        let data = r#"{"prompt_tokens":10,"completion_tokens":15,"total_tokens":25}"#;
        let usage: Usage = serde_json::from_str(data).expect("parse");
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 15);
        assert_eq!(usage.total_tokens, 25);
        assert_eq!(usage.prompt_cache_hit_tokens, 0);
        assert_eq!(usage.prompt_cache_miss_tokens, 0);
        assert_eq!(usage.prompt_cache_write_tokens, 0);
    }

    #[test]
    fn text_content_serializes_as_bare_string_byte_for_byte() {
        // Pins the old `Option<String>` wire shape: a text-only message
        // must still serialize its content as a bare JSON string, not a
        // wrapped object or a one-element array. This is the exact shape
        // `Option<String>` produced before `Content` existed.
        let msg = Message {
            role: Role::User,
            content: Some(Content::text("hello")),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        };
        let json = serde_json::to_string(&msg).expect("serialize");
        assert_eq!(json, r#"{"role":"user","content":"hello"}"#);
    }

    #[test]
    fn absent_content_is_omitted_from_the_wire_exactly_as_before() {
        // The `None` case is real: an assistant message that only calls
        // tools has no content. It must stay entirely absent from the
        // wire, as `skip_serializing_if` already gave it.
        let msg = Message {
            role: Role::Assistant,
            content: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        };
        let json = serde_json::to_string(&msg).expect("serialize");
        assert_eq!(json, r#"{"role":"assistant"}"#);
    }

    #[test]
    fn parts_content_serializes_to_openai_compatible_array() {
        let msg = Message {
            role: Role::User,
            content: Some(Content::Parts(vec![
                ContentPart::Text {
                    text: "look at this".into(),
                },
                ContentPart::ImageUrl {
                    url: "data:image/png;base64,AAA".into(),
                },
            ])),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        };
        let json = serde_json::to_string(&msg).expect("serialize");
        assert_eq!(
            json,
            r#"{"role":"user","content":[{"type":"text","text":"look at this"},{"type":"image_url","image_url":{"url":"data:image/png;base64,AAA"}}]}"#
        );
    }

    #[test]
    fn text_content_round_trips_through_json() {
        let msg = Message {
            role: Role::User,
            content: Some(Content::text("round trip me")),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        };
        let json = serde_json::to_string(&msg).expect("serialize");
        let back: Message = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(
            back.content.as_ref().and_then(Content::as_text),
            Some("round trip me")
        );
    }

    #[test]
    fn a_plain_string_content_field_deserializes_as_text() {
        // An old session file, saved before `Content` existed, has
        // `content` as a bare JSON string. It must still load.
        let json = r#"{"role":"user","content":"from an old session file"}"#;
        let msg: Message = serde_json::from_str(json).expect("deserialize");
        assert_eq!(
            msg.content.as_ref().and_then(Content::as_text),
            Some("from an old session file")
        );
    }

    #[test]
    fn parts_content_round_trips_through_json() {
        let original = Content::Parts(vec![ContentPart::Text { text: "hi".into() }]);
        let json = serde_json::to_string(&original).expect("serialize");
        let back: Content = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, original);
    }
}
