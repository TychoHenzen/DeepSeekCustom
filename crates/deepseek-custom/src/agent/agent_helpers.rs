use tracing::{debug, info};

use crate::api::provider::Provider;
use crate::api::types::{Content, ContentPart, ImageAttachment, Message, Role, ToolCall};

/// Result of building a user's content for an API request: the `Content`
/// to send, and an optional notice to post in the transcript when the
/// image could not be carried on this provider.
pub struct BuiltUserContent {
    pub content: crate::api::types::Content,
    pub notice: Option<String>,
}

/// Aggregate of everything collected from one streaming response.
pub(crate) struct StreamCollection {
    pub text: String,
    pub reasoning: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: String,
    pub usage: Option<crate::api::types::Usage>,
    pub interrupted: bool,
}

/// Low-water mark for the hysteresis oscillator: a third of the budget.
pub fn context_low_water(budget: usize) -> usize {
    budget / 3
}

/// Build the outgoing `Content` for a turn's user message, mapping an
/// optional image attachment onto what `provider` actually accepts.
///
/// DeepSeek accepts no image content part at all. Ollama accepts the
/// OpenAI `image_url` shape on a vision model.
pub fn build_user_content(
    provider: Provider,
    text: &str,
    image: Option<&ImageAttachment>,
) -> BuiltUserContent {
    let Some(image) = image else {
        return BuiltUserContent {
            content: Content::text(text),
            notice: None,
        };
    };
    match provider {
        Provider::DeepSeek => BuiltUserContent {
            content: Content::text(text),
            notice: Some(
                "DeepSeek does not support image attachments; the image \
                 was not sent."
                    .to_string(),
            ),
        },
        Provider::Ollama => BuiltUserContent {
            content: Content::Parts(vec![
                ContentPart::Text {
                    text: text.to_string(),
                },
                ContentPart::ImageUrl {
                    url: format!("data:{};base64,{}", image.media_type, image.data),
                },
            ]),
            notice: None,
        },
    }
}

/// Build an assistant message carrying tool calls.
pub(crate) fn assistant_with_tools(
    text: &str,
    reasoning: &str,
    tool_calls: &[ToolCall],
) -> Message {
    Message {
        role: Role::Assistant,
        content: if text.is_empty() {
            None
        } else {
            Some(Content::text(text))
        },
        tool_calls: Some(tool_calls.to_vec()),
        tool_call_id: None,
        reasoning_content: if reasoning.is_empty() {
            None
        } else {
            Some(reasoning.to_string())
        },
    }
}

/// Merge a streaming tool call delta into the accumulated tool calls list.
///
/// DeepSeek streams tool calls across multiple chunks:
/// - First chunk: `{index: 0, id: "call_xxx",
///   function: {name: "read", arguments: ""}}`
/// - Subsequent chunks: `{index: 0, function: {arguments: "more_json"}}`
///
/// Matches by index and merges partial fields.
pub(crate) fn merge_tool_call(accumulated: &mut Vec<ToolCall>, delta: &ToolCall) {
    let idx = delta.index;

    if let Some(existing) = accumulated.iter_mut().find(|tc| tc.index == idx) {
        merge_into_existing(existing, delta);
        return;
    }

    let mut tc = delta.clone();
    let func = tc.function.get_or_insert_with(Default::default);
    if func.arguments.is_none() {
        func.arguments = Some(String::new());
    }
    debug!(
        index = ?idx,
        name = ?func.name,
        args_len = func.arguments.as_ref().map_or(0, |a| a.len()),
        "merge_tool_call: new"
    );
    accumulated.push(tc);
}

fn merge_into_existing(existing: &mut ToolCall, delta: &ToolCall) {
    if existing.id.is_empty() && !delta.id.is_empty() {
        existing.id = delta.id.clone();
    }
    let Some(ref delta_func) = delta.function else {
        return;
    };
    let existing_func = existing.function.get_or_insert_with(Default::default);
    if let Some(ref name) = delta_func.name
        && existing_func.name.is_none()
    {
        debug!(
            index = ?delta.index,
            name = %name,
            "merge_tool_call: set name"
        );
        existing_func.name = Some(name.clone());
    }
    if let Some(ref args) = delta_func.arguments {
        if let Some(ref mut existing_args) = existing_func.arguments {
            existing_args.push_str(args);
        } else {
            existing_func.arguments = Some(args.clone());
        }
    }
}

/// Drop tool calls that lack a function name and log how many got
/// filtered.
pub(crate) fn filter_valid_tool_calls(tool_calls: &[ToolCall]) -> Vec<ToolCall> {
    let valid: Vec<ToolCall> = tool_calls
        .iter()
        .filter(|tc| tc.function.as_ref().and_then(|f| f.name.as_ref()).is_some())
        .cloned()
        .collect();
    let filtered_out = tool_calls.len() - valid.len();
    if filtered_out > 0 {
        info!(
            total = tool_calls.len(),
            valid = valid.len(),
            "filtered out {filtered_out} nameless tool call(s)"
        );
    }
    valid
}
