use serde::{Deserialize, Serialize};

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
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
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: FunctionCall,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
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
}

#[derive(Debug, Clone, Deserialize)]
pub struct StreamChunk {
    pub id: Option<String>,
    pub object: Option<String>,
    pub created: Option<u64>,
    pub model: Option<String>,
    pub choices: Option<Vec<StreamChoice>>,
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
                content: Some("hello".into()),
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
        };

        let json = serde_json::to_string(&req).expect("serialize");
        let parsed: serde_json::Value =
            serde_json::from_str(&json).expect("valid JSON");

        assert_eq!(parsed["model"], "deepseek-v4-flash");
        assert_eq!(parsed["messages"][0]["role"], "user");
        assert_eq!(parsed["messages"][0]["content"], "hello");
        assert_eq!(parsed["stream"], false);
        assert_eq!(parsed["temperature"], 0.7);
        assert_eq!(parsed["max_tokens"], 1024);
        assert!(parsed.get("thinking").is_none());
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
            response.choices[0].message.content.as_deref(),
            Some("Hello! I'm DeepSeek, an AI assistant. How can I help you today?")
        );
        assert_eq!(response.usage.as_ref().unwrap().prompt_tokens, 10);
        assert_eq!(response.usage.as_ref().unwrap().completion_tokens, 15);
        assert_eq!(response.usage.as_ref().unwrap().total_tokens, 25);
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
}
