//! Relevance scoring for conversation messages before a prune pass.
//!
//! `build_index` and `parse_scores` are pure and offline, so they are
//! testable without a network. `score_messages` is the only piece that
//! talks to the API, and it never returns an error: a scoring failure
//! must leave the calling turn running, so every failure path logs a
//! `warn` and returns `None` instead.

use serde::Serialize;
use serde_json::Value;
use tracing::warn;

use crate::agent::history::estimate_message_tokens;
use crate::api::client::{ApiClient, Provider};
use crate::api::types::{ChatRequest, Content, Message};

const SYSTEM_PROMPT: &str = "You are scoring a conversation history that is about to be \
trimmed to save tokens. For each entry, score from 0.0 to 1.0 how much its full content is \
worth keeping. Raw file contents and command output usually score low once they have already \
been summarized in later messages. A decision, a conclusion, or a summary of work done scores \
high. Reply with a JSON array of objects shaped like {\"id\":0,\"score\":0.5} and nothing else.";

#[derive(Serialize)]
struct IndexEntry<'a> {
    id: usize,
    role: &'a str,
    tokens: usize,
    preview: String,
}

/// Build a compact JSON index of `messages` for the model to rank. Never
/// includes full message bodies, only a 100-character preview.
pub fn build_index(messages: &[Message]) -> String {
    let entries: Vec<IndexEntry> = messages
        .iter()
        .enumerate()
        .map(|(id, msg)| IndexEntry {
            id,
            role: role_name(msg),
            tokens: estimate_message_tokens(msg),
            preview: build_preview(msg),
        })
        .collect();
    serde_json::to_string(&entries).expect("index entries always serialize")
}

fn role_name(msg: &Message) -> &'static str {
    match msg.role {
        crate::api::types::Role::System => "system",
        crate::api::types::Role::User => "user",
        crate::api::types::Role::Assistant => "assistant",
        crate::api::types::Role::Tool => "tool",
    }
}

/// First 100 characters of the message content, newlines collapsed to
/// spaces and consecutive whitespace squeezed. Falls back to a preview of
/// tool call names when there is no content, and to an empty string when
/// there is nothing to preview.
fn build_preview(msg: &Message) -> String {
    if let Some(text) = msg.content.as_ref().and_then(Content::as_text) {
        return collapse_whitespace(text).chars().take(100).collect();
    }
    if let Some(tool_calls) = &msg.tool_calls {
        let names: Vec<&str> = tool_calls
            .iter()
            .filter_map(|tc| tc.function.as_ref())
            .filter_map(|f| f.name.as_deref())
            .collect();
        if !names.is_empty() {
            return format!("calls: {}", names.join(", "));
        }
    }
    String::new()
}

/// Collapse newlines and consecutive whitespace into single spaces.
fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Parse a model reply into a `Vec<f32>` of scores indexed by message id.
///
/// Tolerates a markdown code fence or surrounding prose by locating the
/// outermost `[` and its matching `]`. Returns `None` when the JSON does
/// not parse, an id is out of range or missing, an id repeats, or any
/// score is not finite.
pub fn parse_scores(reply: &str, expected_len: usize) -> Option<Vec<f32>> {
    let span = extract_array_span(reply)?;
    let raw: Vec<Value> = serde_json::from_str(span).ok()?;

    let mut scores: Vec<Option<f32>> = vec![None; expected_len];
    for entry in raw {
        let id = entry.get("id")?.as_u64()? as usize;
        let score = entry.get("score")?.as_f64()? as f32;
        if !score.is_finite() || id >= expected_len || scores[id].is_some() {
            return None;
        }
        scores[id] = Some(score.clamp(0.0, 1.0));
    }
    scores.into_iter().collect()
}

/// Find the outermost `[...]` span in `text`.
fn extract_array_span(text: &str) -> Option<&str> {
    let start = text.find('[')?;
    let end = text.rfind(']')?;
    if end < start {
        return None;
    }
    Some(&text[start..=end])
}

/// The model a scoring call runs on for this provider.
///
/// DeepSeek gets `deepseek-v4-flash`, since this is a ranking job over
/// short previews rather than the main conversation, and the cheaper
/// model is enough for it. Any other provider gets the conversation
/// model itself: a DeepSeek model name would just fail there, and an
/// Ollama call is local anyway, so there is nothing to save.
pub fn scoring_model(provider: Provider, conversation_model: &str) -> String {
    match provider {
        Provider::DeepSeek => "deepseek-v4-flash".to_string(),
        Provider::Ollama => conversation_model.to_string(),
    }
}

/// Ask the model to score every message in `messages` for how worth
/// keeping it is. Runs on whatever `scoring_model` picks for this
/// client's provider. Never panics and never propagates an error: any
/// failure is logged at `warn` and reported as `None`, so a scoring
/// failure leaves the calling turn running.
pub async fn score_messages(
    client: &ApiClient,
    messages: &[Message],
    conversation_model: &str,
) -> Option<Vec<f32>> {
    if messages.is_empty() {
        return Some(vec![]);
    }

    let index = build_index(messages);
    let req = ChatRequest {
        model: scoring_model(client.provider(), conversation_model),
        messages: vec![
            Message::system(SYSTEM_PROMPT.to_string()),
            Message::user(index),
        ],
        tools: None,
        tool_choice: None,
        stream: false,
        temperature: Some(0.0),
        max_tokens: Some((messages.len() as u32) * 20 + 64),
        thinking: None,
        thinking_mode: None,
        reasoning_effort: None,
        effort: Some(crate::effort::Effort::None),
    };

    let response = match client.chat(&req).await {
        Ok(r) => r,
        Err(e) => {
            warn!("relevance scoring: request failed: {e}");
            return None;
        }
    };

    let Some(choice) = response.choices.first() else {
        warn!("relevance scoring: empty choices in response");
        return None;
    };
    let Some(content) = choice.message.content.as_ref().and_then(Content::as_text) else {
        warn!("relevance scoring: no content in response");
        return None;
    };

    match parse_scores(content, messages.len()) {
        Some(scores) => Some(scores),
        None => {
            warn!("relevance scoring: failed to parse model reply into scores");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::{FunctionCall, Role, ToolCall};

    fn tool_call(name: &str) -> ToolCall {
        ToolCall {
            id: "call1".into(),
            call_type: "function".into(),
            function: Some(FunctionCall {
                name: Some(name.into()),
                arguments: Some("{}".into()),
            }),
            index: None,
        }
    }

    #[test]
    fn build_index_emits_one_entry_per_message_with_ids_and_roles() {
        let messages = vec![
            Message::user("hi".into()),
            Message::assistant("hello".into()),
            Message::tool_result("call1".into(), "result".into()),
        ];
        let index = build_index(&messages);
        let parsed: Value = serde_json::from_str(&index).expect("valid JSON");
        let arr = parsed.as_array().expect("array");
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["id"], 0);
        assert_eq!(arr[0]["role"], "user");
        assert_eq!(arr[1]["id"], 1);
        assert_eq!(arr[1]["role"], "assistant");
        assert_eq!(arr[2]["id"], 2);
        assert_eq!(arr[2]["role"], "tool");
    }

    #[test]
    fn build_index_truncates_long_preview_and_collapses_newlines() {
        let long_text = format!("line one\nline two\n{}", "x".repeat(200));
        let messages = vec![Message::user(long_text)];
        let index = build_index(&messages);
        let parsed: Value = serde_json::from_str(&index).expect("valid JSON");
        let preview = parsed[0]["preview"].as_str().expect("preview string");
        assert_eq!(preview.chars().count(), 100);
        assert!(!preview.contains('\n'));
        assert!(preview.starts_with("line one line two"));
    }

    #[test]
    fn build_index_previews_tool_call_names_when_no_content() {
        let msg = Message {
            role: Role::Assistant,
            content: None,
            tool_calls: Some(vec![tool_call("bash"), tool_call("read")]),
            tool_call_id: None,
            reasoning_content: None,
        };
        let index = build_index(&[msg]);
        let parsed: Value = serde_json::from_str(&index).expect("valid JSON");
        assert_eq!(parsed[0]["preview"], "calls: bash, read");
    }

    #[test]
    fn build_index_previews_empty_string_when_nothing_to_preview() {
        let msg = Message {
            role: Role::Assistant,
            content: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        };
        let index = build_index(&[msg]);
        let parsed: Value = serde_json::from_str(&index).expect("valid JSON");
        assert_eq!(parsed[0]["preview"], "");
    }

    #[test]
    fn parse_scores_handles_clean_json_array() {
        let reply = r#"[{"id":0,"score":0.2},{"id":1,"score":0.9}]"#;
        let scores = parse_scores(reply, 2).expect("parses");
        assert_eq!(scores, vec![0.2, 0.9]);
    }

    #[test]
    fn parse_scores_handles_markdown_code_fence() {
        let reply = "```json\n[{\"id\":0,\"score\":0.3}]\n```";
        let scores = parse_scores(reply, 1).expect("parses");
        assert_eq!(scores, vec![0.3]);
    }

    #[test]
    fn parse_scores_handles_prose_around_array() {
        let reply = "Here are the scores: [{\"id\":0,\"score\":0.4}] hope that helps!";
        let scores = parse_scores(reply, 1).expect("parses");
        assert_eq!(scores, vec![0.4]);
    }

    #[test]
    fn parse_scores_returns_none_on_malformed_json() {
        let reply = "[{\"id\":0,\"score\":]";
        assert!(parse_scores(reply, 1).is_none());
    }

    #[test]
    fn parse_scores_returns_none_when_id_missing() {
        let reply = r#"[{"id":0,"score":0.5}]"#;
        assert!(parse_scores(reply, 2).is_none());
    }

    #[test]
    fn parse_scores_returns_none_on_out_of_range_id() {
        let reply = r#"[{"id":0,"score":0.5},{"id":5,"score":0.5}]"#;
        assert!(parse_scores(reply, 2).is_none());
    }

    #[test]
    fn parse_scores_returns_none_on_duplicate_id() {
        let reply = r#"[{"id":0,"score":0.5},{"id":0,"score":0.9}]"#;
        assert!(parse_scores(reply, 1).is_none());
    }

    #[test]
    fn parse_scores_returns_none_on_non_finite_score() {
        // JSON has no NaN literal. A literal that big is valid JSON syntax
        // but overflows to infinity once read as an f64. That is the only
        // way a reply reaches the non-finite check, so it is what this
        // test uses.
        let inf_reply = r#"[{"id":0,"score":1e400}]"#;
        assert!(parse_scores(inf_reply, 1).is_none());
    }

    #[test]
    fn parse_scores_clamps_out_of_range_scores() {
        let reply = r#"[{"id":0,"score":1.5},{"id":1,"score":-0.5}]"#;
        let scores = parse_scores(reply, 2).expect("parses");
        assert_eq!(scores, vec![1.0, 0.0]);
    }

    #[test]
    fn scoring_model_uses_flash_for_deepseek() {
        let model = scoring_model(Provider::DeepSeek, "deepseek-v4-pro");
        assert_eq!(model, "deepseek-v4-flash");
    }

    #[test]
    fn scoring_model_uses_the_conversation_model_for_ollama() {
        let model = scoring_model(Provider::Ollama, "qwen2.5-coder:7b-instruct-q4_K_M");
        assert_eq!(model, "qwen2.5-coder:7b-instruct-q4_K_M");
    }
}
