use crate::agent::pruning;
pub use crate::agent::pruning::PruneReport;
use crate::api::types::Message;

/// Thread-safe conversation history with approximate token tracking.
pub struct MessageHistory {
    system_prompt: String,
    /// Optional per-turn addition to the system prompt, appended after a
    /// blank line. Meant for instructions that come and go with a runtime
    /// setting (e.g. a voice-mode toggle), without rebuilding the whole
    /// system prompt each time.
    system_suffix: Option<String>,
    messages: Vec<Message>,
    token_count: usize,
}

impl MessageHistory {
    pub fn new(system_prompt: String) -> Self {
        let token_count = estimate_tokens(&system_prompt);
        Self {
            system_prompt,
            system_suffix: None,
            messages: Vec::new(),
            token_count,
        }
    }

    /// Add a message to the conversation.
    pub fn push(&mut self, msg: Message) {
        let tok = estimate_message_tokens(&msg);
        self.token_count += tok;
        self.messages.push(msg);
    }

    /// Set or clear the per-turn system prompt suffix. Recomputes the
    /// tracked token count so `estimated_tokens()` stays accurate.
    pub fn set_system_suffix(&mut self, suffix: Option<String>) {
        let old_tokens = suffix_tokens(self.system_suffix.as_deref());
        let new_tokens = suffix_tokens(suffix.as_deref());
        self.token_count = self.token_count - old_tokens + new_tokens;
        self.system_suffix = suffix;
    }

    /// Iterate over all messages (system prompt NOT included).
    pub fn iter(&self) -> impl Iterator<Item = &Message> {
        self.messages.iter()
    }

    /// Number of messages in history (excluding system prompt).
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    /// Whether the history has any messages.
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Clear all messages (keep system prompt and its suffix).
    pub fn clear(&mut self) {
        self.messages.clear();
        self.token_count =
            estimate_tokens(&self.system_prompt) + suffix_tokens(self.system_suffix.as_deref());
    }

    /// Return messages in API-ready format (system prompt first). When a
    /// suffix is set, it is appended after a blank line. With no suffix set,
    /// the system message is unchanged from the base prompt.
    pub fn to_api_messages(&self) -> Vec<Message> {
        let mut out = Vec::with_capacity(self.messages.len() + 1);
        let system_content = match &self.system_suffix {
            Some(suffix) => format!("{}{}", self.system_prompt, suffix_addition(suffix)),
            None => self.system_prompt.clone(),
        };
        out.push(Message::system(system_content));
        out.extend(self.messages.clone());
        out
    }

    /// Approximate token count (system prompt + all messages).
    /// Uses rough heuristic: 1 token ≈ 4 characters for English text.
    pub fn estimated_tokens(&self) -> usize {
        self.token_count
    }

    /// Prune down to `low_water_tokens` via the three tiers in `pruning.rs`.
    /// A no-op under budget. See `pruning::prune_to_budget` for details.
    pub fn prune_to_budget(
        &mut self,
        low_water_tokens: usize,
        scores: Option<&[f32]>,
    ) -> PruneReport {
        let base_tokens =
            estimate_tokens(&self.system_prompt) + suffix_tokens(self.system_suffix.as_deref());
        let report =
            pruning::prune_to_budget(&mut self.messages, base_tokens, low_water_tokens, scores);
        self.recompute_token_count();
        report
    }

    /// Recompute `token_count` from scratch. The incremental count
    /// `push` and `clear` keep cannot survive pruning's removals.
    fn recompute_token_count(&mut self) {
        let base_tokens =
            estimate_tokens(&self.system_prompt) + suffix_tokens(self.system_suffix.as_deref());
        let messages_tokens: usize = self.messages.iter().map(estimate_message_tokens).sum();
        self.token_count = base_tokens + messages_tokens;
    }
}

/// Estimate tokens from a string: ~4 chars per token.
fn estimate_tokens(s: &str) -> usize {
    s.chars().count().div_ceil(4)
}

/// The text a suffix contributes to the system message: a blank line
/// followed by the suffix itself. Matches the format `to_api_messages`
/// emits, so token counts stay in sync with the real output.
fn suffix_addition(suffix: &str) -> String {
    format!("\n\n{}", suffix)
}

/// Token count a suffix adds to the system message, or zero when unset.
fn suffix_tokens(suffix: Option<&str>) -> usize {
    match suffix {
        Some(s) => estimate_tokens(&suffix_addition(s)),
        None => 0,
    }
}

pub(crate) fn estimate_message_tokens(msg: &Message) -> usize {
    let mut chars = 0;
    if let Some(ref c) = msg.content {
        chars += c.chars().count();
    }
    if let Some(ref r) = msg.reasoning_content {
        chars += r.chars().count();
    }
    if let Some(ref tcs) = msg.tool_calls {
        for tc in tcs {
            if let Some(ref func) = tc.function {
                if let Some(ref name) = func.name {
                    chars += name.chars().count();
                }
                if let Some(ref args) = func.arguments {
                    chars += args.chars().count();
                }
            }
        }
    }
    chars.div_ceil(4)
}

// Convenience constructors for Message (keeps types.rs clean)
impl Message {
    pub fn system(content: String) -> Self {
        Self {
            role: crate::api::types::Role::System,
            content: Some(content),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    pub fn user(content: String) -> Self {
        Self {
            role: crate::api::types::Role::User,
            content: Some(content),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    pub fn assistant(content: String) -> Self {
        Self {
            role: crate::api::types::Role::Assistant,
            content: Some(content),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    pub fn tool_result(tool_call_id: String, content: String) -> Self {
        Self {
            role: crate::api::types::Role::Tool,
            content: Some(content),
            tool_calls: None,
            tool_call_id: Some(tool_call_id),
            reasoning_content: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_increases_count_and_tokens() {
        let mut h = MessageHistory::new("You are helpful.".into());
        assert_eq!(h.len(), 0);
        h.push(Message::user("hello".into()));
        assert_eq!(h.len(), 1);
        assert!(h.estimated_tokens() > 0);
    }

    #[test]
    fn clear_drops_messages_keeps_system() {
        let mut h = MessageHistory::new("system".into());
        h.push(Message::user("hi".into()));
        h.clear();
        assert_eq!(h.len(), 0);
        assert!(h.is_empty());
    }

    #[test]
    fn to_api_messages_includes_system_first() {
        let h = MessageHistory::new("sys".into());
        let api = h.to_api_messages();
        assert_eq!(api.len(), 1);
        assert_eq!(api[0].role, crate::api::types::Role::System);
    }

    #[test]
    fn estimated_tokens_increases_with_content() {
        let mut h = MessageHistory::new("short".into());
        let before = h.estimated_tokens();
        h.push(Message::user(
            "a long message with many characters and more words".into(),
        ));
        assert!(h.estimated_tokens() > before);
    }

    #[test]
    fn no_suffix_leaves_system_message_unchanged() {
        let h = MessageHistory::new("You are helpful.".into());
        let api = h.to_api_messages();
        assert_eq!(api[0].content.as_deref(), Some("You are helpful."));
    }

    #[test]
    fn set_suffix_appears_in_system_message() {
        let mut h = MessageHistory::new("You are helpful.".into());
        h.set_system_suffix(Some("Reply briefly.".into()));
        let api = h.to_api_messages();
        assert_eq!(
            api[0].content.as_deref(),
            Some("You are helpful.\n\nReply briefly.")
        );
    }

    #[test]
    fn set_suffix_none_removes_it() {
        let mut h = MessageHistory::new("You are helpful.".into());
        h.set_system_suffix(Some("Reply briefly.".into()));
        h.set_system_suffix(None);
        let api = h.to_api_messages();
        assert_eq!(api[0].content.as_deref(), Some("You are helpful."));
    }

    #[test]
    fn suffix_survives_clear() {
        let mut h = MessageHistory::new("You are helpful.".into());
        h.set_system_suffix(Some("Reply briefly.".into()));
        h.push(Message::user("hi".into()));
        h.clear();
        let api = h.to_api_messages();
        assert_eq!(
            api[0].content.as_deref(),
            Some("You are helpful.\n\nReply briefly.")
        );
    }

    #[test]
    fn estimated_tokens_reflects_set_suffix() {
        let mut h = MessageHistory::new("short".into());
        let before = h.estimated_tokens();
        h.set_system_suffix(Some(
            "a long suffix with many characters and more words".into(),
        ));
        assert!(h.estimated_tokens() > before);
    }

    #[test]
    fn estimated_tokens_matches_fresh_recompute_after_prune() {
        let mut h = MessageHistory::new("system prompt".into());
        for i in 0..8 {
            h.push(Message::user(format!("question {i}")));
            h.push(Message::assistant(format!("answer {i}")));
        }
        h.prune_to_budget(1, None);
        let tracked = h.estimated_tokens();
        h.recompute_token_count();
        assert_eq!(h.estimated_tokens(), tracked);
    }
}
