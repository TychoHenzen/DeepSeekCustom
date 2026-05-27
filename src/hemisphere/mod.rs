use crate::agent::history::MessageHistory;

/// Configuration for the dual-agent hemisphere system.
#[derive(Debug, Clone)]
pub struct HemisphereConfig {
    pub enabled: bool,
    pub left_model: String,
    pub right_model: Option<String>,
    pub right_context_window: usize,
    pub right_max_response: usize,
}

impl Default for HemisphereConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            left_model: "deepseek-v4-flash".into(),
            right_model: None,
            right_context_window: 4000,
            right_max_response: 200,
        }
    }
}

/// Right hemisphere system prompt template.
pub const RIGHT_SYSTEM_PROMPT: &str = r#"You are a background advisor watching a coding session.
You see a compressed summary of the conversation.
Your role: detect issues, suggest alternatives, request clarification.
Keep responses short (max 200 tokens).
To request clarification from the main agent, use the clarifier tool.
Be concise. Only speak when you have something useful to add."#;

/// Left hemisphere system prompt template (appended to standard agent prompt).
pub const LEFT_EXTRA_PROMPT: &str = r#"
You are the primary agent. A background advisor (right hemisphere) may occasionally
ask clarifying questions. When this happens, you will see messages prefixed with
"Right hemisphere asks:". Consider these questions carefully before responding."#;

/// Compress conversation history for the right hemisphere.
///
/// Strategy: keep system prompt abbreviated, summarize early turns,
/// keep last N turns verbatim.
pub fn compress_for_right(history: &MessageHistory, keep_verbatim: usize) -> String {
    let messages = history.to_api_messages();
    let total = messages.len();

    // Skip system prompt — right hemisphere has its own
    let user_messages = total.saturating_sub(1);

    if user_messages == 0 {
        return "(empty conversation)".into();
    }

    let mut parts: Vec<String> = Vec::new();

    if user_messages > keep_verbatim {
        let early_count = user_messages - keep_verbatim;
        parts.push(format!(
            "[Summary of earlier conversation: {early_count} messages exchanged. \
             The user asked a coding question and the assistant has been \
             reading files, executing commands, and writing code.]"
        ));
    }

    // Last N turns verbatim (skip system prompt at index 0)
    let start = if user_messages > keep_verbatim {
        total - keep_verbatim
    } else {
        1 // skip system prompt
    };
    for msg in &messages[start..] {
        let role = format!("{:?}", msg.role).to_lowercase();
        if let Some(ref content) = msg.content {
            // Truncate long messages
            let truncated = if content.len() > 500 {
                format!("{}...", &content[..500])
            } else {
                content.clone()
            };
            parts.push(format!("[{role}] {truncated}"));
        }
    }

    parts.join("\n\n")
}

/// Estimate if content fits within the right hemisphere's context window.
pub fn fits_context_window(content: &str, max_tokens: usize) -> bool {
    // Rough: 1 token ≈ 4 chars
    content.chars().count() / 4 <= max_tokens
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::{Message, Role};

    #[test]
    fn hemisphere_config_defaults_disabled() {
        let cfg = HemisphereConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.right_context_window, 4000);
        assert_eq!(cfg.right_max_response, 200);
    }

    #[test]
    fn compress_empty_history() {
        let h = MessageHistory::new("system".into());
        let result = compress_for_right(&h, 3);
        assert!(result.contains("empty conversation"));
    }

    #[test]
    fn compress_reduces_history() {
        let mut h = MessageHistory::new("system".into());
        for i in 0..10 {
            h.push(Message::user(format!("message {i}")));
        }
        let result = compress_for_right(&h, 3);
        assert!(result.contains("Summary of earlier conversation"));
        assert!(result.contains("message 9")); // last verbatim
        assert!(!result.contains("message 0")); // summarized away
    }

    #[test]
    fn fits_context_detects_overflow() {
        let small = "short";
        assert!(fits_context_window(small, 100));

        let large = "x".repeat(10000);
        assert!(!fits_context_window(&large, 100));
    }
}
