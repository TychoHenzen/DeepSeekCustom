use crate::agent::pruning;
pub use crate::agent::pruning::PruneReport;
use crate::api::types::{Content, Message};

/// Thread-safe conversation history with approximate token tracking.
pub struct MessageHistory {
    system_prompt: String,
    /// Optional per-turn addition to the system prompt, appended after a
    /// blank line. Meant for instructions that come and go with a runtime
    /// setting (e.g. a voice-mode toggle), without rebuilding the whole
    /// system prompt each time.
    system_suffix: Option<String>,
    /// The working directory line, re-read every turn by
    /// `AgentLoop::sync_dynamic_config` from the same shared value the
    /// `Bash`, `Read`, and `Write` tools act against. `None` until the
    /// first sync. Kept separate from `system_suffix` so a directory
    /// change and the voice-mode suffix can both be present in the system
    /// message at once, neither one overwriting the other.
    working_dir: Option<String>,
    messages: Vec<Message>,
    token_count: usize,
}

impl MessageHistory {
    pub fn new(system_prompt: String) -> Self {
        let token_count = estimate_tokens(&system_prompt);
        Self {
            system_prompt,
            system_suffix: None,
            working_dir: None,
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

    /// Set or clear the reported working directory. Called every turn by
    /// `AgentLoop::sync_dynamic_config`, so a directory change takes effect
    /// on the next turn's system prompt with no restart. Recomputes the
    /// tracked token count so `estimated_tokens()` stays accurate.
    pub fn set_working_dir(&mut self, working_dir: Option<String>) {
        let old_tokens = working_dir_tokens(self.working_dir.as_deref());
        let new_tokens = working_dir_tokens(working_dir.as_deref());
        self.token_count = self.token_count - old_tokens + new_tokens;
        self.working_dir = working_dir;
    }

    /// The base system prompt, without any per-turn suffix applied.
    pub fn system_prompt(&self) -> &str {
        &self.system_prompt
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

    /// Clear all messages (keep system prompt, working directory, and suffix).
    pub fn clear(&mut self) {
        self.messages.clear();
        self.token_count = self.base_tokens();
    }

    /// Return messages in API-ready format (system prompt first). The
    /// working directory line comes first, when set, then the voice-mode
    /// suffix, when set. With neither set, the system message is unchanged
    /// from the base prompt.
    pub fn to_api_messages(&self) -> Vec<Message> {
        let mut out = Vec::with_capacity(self.messages.len() + 1);
        let mut system_content = self.system_prompt.clone();
        if let Some(dir) = &self.working_dir {
            system_content.push_str(&working_dir_addition(dir));
        }
        if let Some(suffix) = &self.system_suffix {
            system_content.push_str(&suffix_addition(suffix));
        }
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
        let base_tokens = self.base_tokens();
        let report =
            pruning::prune_to_budget(&mut self.messages, base_tokens, low_water_tokens, scores);
        self.recompute_token_count();
        report
    }

    /// Replace the whole message vector with a saved one, for restoring a
    /// session from disk. Three guarantees:
    /// - Replaces, never appends: any existing messages are dropped first.
    /// - Leaves the system prompt and system suffix untouched. A saved
    ///   conversation reopens under whatever prompt the current agent runs,
    ///   not the one it was saved under.
    /// - Recomputes the token count from scratch via `recompute_token_count`,
    ///   the same way pruning does, rather than adjusting it incrementally.
    pub fn restore(&mut self, messages: Vec<Message>) {
        self.messages = messages;
        self.recompute_token_count();
    }

    /// Recompute `token_count` from scratch. The incremental count
    /// `push` and `clear` keep cannot survive pruning's removals.
    fn recompute_token_count(&mut self) {
        let base_tokens = self.base_tokens();
        let messages_tokens: usize = self.messages.iter().map(estimate_message_tokens).sum();
        self.token_count = base_tokens + messages_tokens;
    }

    /// Token count of the system message alone: the base prompt plus the
    /// working directory line and the voice-mode suffix, whichever of the
    /// two are currently set.
    fn base_tokens(&self) -> usize {
        estimate_tokens(&self.system_prompt)
            + working_dir_tokens(self.working_dir.as_deref())
            + suffix_tokens(self.system_suffix.as_deref())
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

/// The text the working directory line contributes to the system message:
/// a blank line followed by a "Working directory: ..." sentence. Matches
/// the format `to_api_messages` emits, so token counts stay in sync with
/// the real output.
fn working_dir_addition(working_dir: &str) -> String {
    format!("\n\nWorking directory: {}.", working_dir)
}

/// Token count the working directory line adds to the system message, or
/// zero when unset.
fn working_dir_tokens(working_dir: Option<&str>) -> usize {
    match working_dir {
        Some(d) => estimate_tokens(&working_dir_addition(d)),
        None => 0,
    }
}

pub(crate) fn estimate_message_tokens(msg: &Message) -> usize {
    let mut chars = 0;
    if let Some(ref c) = msg.content {
        chars += content_chars(c);
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

/// Character count of a message's content, for the token estimate above.
/// A `Text` message counts its own characters, matching the old
/// `Option<String>` behaviour exactly. A `Parts` message sums the
/// characters of its text parts plus the characters of every image part's
/// `url` field, base64 payload included. The same ~4-chars-per-token rule
/// this module already uses for plain text is applied to that payload too.
/// It is not a real per-tile vision token count, but it is not meant to
/// be one: it only needs to make an unelided image register as large in
/// the budget, which a base64 payload of any real screenshot does. An
/// image that counted as zero tokens, the prior behaviour, could never be
/// the thing that pushes a conversation over budget, and eliding it would
/// never show up as a token saving either.
fn content_chars(content: &Content) -> usize {
    match content {
        Content::Text(s) => s.chars().count(),
        Content::Parts(parts) => parts
            .iter()
            .map(|p| match p {
                crate::api::types::ContentPart::Text { text } => text.chars().count(),
                crate::api::types::ContentPart::ImageUrl { url } => url.chars().count(),
            })
            .sum(),
    }
}

// Convenience constructors for Message (keeps types.rs clean)
impl Message {
    pub fn system(content: String) -> Self {
        Self {
            role: crate::api::types::Role::System,
            content: Some(Content::text(content)),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    pub fn user(content: String) -> Self {
        Self {
            role: crate::api::types::Role::User,
            content: Some(Content::text(content)),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    pub fn assistant(content: String) -> Self {
        Self {
            role: crate::api::types::Role::Assistant,
            content: Some(Content::text(content)),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    pub fn tool_result(tool_call_id: String, content: String) -> Self {
        Self {
            role: crate::api::types::Role::Tool,
            content: Some(Content::text(content)),
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
        assert_eq!(api[0].content.as_ref().and_then(Content::as_text), Some("You are helpful."));
    }

    #[test]
    fn set_suffix_appears_in_system_message() {
        let mut h = MessageHistory::new("You are helpful.".into());
        h.set_system_suffix(Some("Reply briefly.".into()));
        let api = h.to_api_messages();
        assert_eq!(
            api[0].content.as_ref().and_then(Content::as_text),
            Some("You are helpful.\n\nReply briefly.")
        );
    }

    #[test]
    fn set_suffix_none_removes_it() {
        let mut h = MessageHistory::new("You are helpful.".into());
        h.set_system_suffix(Some("Reply briefly.".into()));
        h.set_system_suffix(None);
        let api = h.to_api_messages();
        assert_eq!(api[0].content.as_ref().and_then(Content::as_text), Some("You are helpful."));
    }

    #[test]
    fn suffix_survives_clear() {
        let mut h = MessageHistory::new("You are helpful.".into());
        h.set_system_suffix(Some("Reply briefly.".into()));
        h.push(Message::user("hi".into()));
        h.clear();
        let api = h.to_api_messages();
        assert_eq!(
            api[0].content.as_ref().and_then(Content::as_text),
            Some("You are helpful.\n\nReply briefly.")
        );
    }

    #[test]
    fn no_working_dir_leaves_system_message_unchanged() {
        let h = MessageHistory::new("You are helpful.".into());
        let api = h.to_api_messages();
        assert_eq!(api[0].content.as_ref().and_then(Content::as_text), Some("You are helpful."));
    }

    #[test]
    fn set_working_dir_appears_in_system_message() {
        let mut h = MessageHistory::new("You are helpful.".into());
        h.set_working_dir(Some("C:\\proj".into()));
        let api = h.to_api_messages();
        assert_eq!(
            api[0].content.as_ref().and_then(Content::as_text),
            Some("You are helpful.\n\nWorking directory: C:\\proj.")
        );
    }

    #[test]
    fn set_working_dir_none_removes_it() {
        let mut h = MessageHistory::new("You are helpful.".into());
        h.set_working_dir(Some("C:\\proj".into()));
        h.set_working_dir(None);
        let api = h.to_api_messages();
        assert_eq!(api[0].content.as_ref().and_then(Content::as_text), Some("You are helpful."));
    }

    #[test]
    fn working_dir_and_suffix_both_appear_together() {
        let mut h = MessageHistory::new("You are helpful.".into());
        h.set_working_dir(Some("C:\\proj".into()));
        h.set_system_suffix(Some("Reply briefly.".into()));
        let api = h.to_api_messages();
        assert_eq!(
            api[0].content.as_ref().and_then(Content::as_text),
            Some("You are helpful.\n\nWorking directory: C:\\proj.\n\nReply briefly.")
        );
    }

    #[test]
    fn changing_working_dir_replaces_the_reported_directory() {
        let mut h = MessageHistory::new("You are helpful.".into());
        h.set_working_dir(Some("C:\\proj".into()));
        h.set_working_dir(Some("C:\\other".into()));
        let api = h.to_api_messages();
        assert_eq!(
            api[0].content.as_ref().and_then(Content::as_text),
            Some("You are helpful.\n\nWorking directory: C:\\other.")
        );
    }

    #[test]
    fn working_dir_survives_clear() {
        let mut h = MessageHistory::new("You are helpful.".into());
        h.set_working_dir(Some("C:\\proj".into()));
        h.push(Message::user("hi".into()));
        h.clear();
        let api = h.to_api_messages();
        assert_eq!(
            api[0].content.as_ref().and_then(Content::as_text),
            Some("You are helpful.\n\nWorking directory: C:\\proj.")
        );
    }

    #[test]
    fn estimated_tokens_reflects_set_working_dir() {
        let mut h = MessageHistory::new("short".into());
        let before = h.estimated_tokens();
        h.set_working_dir(Some("a long directory path with many characters".into()));
        assert!(h.estimated_tokens() > before);
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
    fn restore_replaces_rather_than_appends() {
        let mut h = MessageHistory::new("sys".into());
        h.push(Message::user("old1".into()));
        h.push(Message::user("old2".into()));
        let restored = vec![Message::user("new1".into()), Message::assistant("new2".into())];
        h.restore(restored.clone());
        assert_eq!(h.len(), restored.len());
        let got: Vec<&Message> = h.iter().collect();
        for (g, r) in got.iter().zip(restored.iter()) {
            assert_eq!(g.content, r.content);
            assert_eq!(g.role, r.role);
        }
    }

    #[test]
    fn restore_token_count_matches_pushed_one_at_a_time() {
        let messages = vec![
            Message::user("hello there".into()),
            Message::assistant("a fairly long reply with several words".into()),
        ];

        let mut restored_h = MessageHistory::new("system prompt".into());
        restored_h.restore(messages.clone());

        let mut pushed_h = MessageHistory::new("system prompt".into());
        for m in messages {
            pushed_h.push(m);
        }

        assert_eq!(restored_h.estimated_tokens(), pushed_h.estimated_tokens());
    }

    #[test]
    fn restore_leaves_system_prompt_unchanged() {
        let mut h = MessageHistory::new("You are helpful.".into());
        h.restore(vec![Message::user("hi".into())]);
        let api = h.to_api_messages();
        assert_eq!(api[0].role, crate::api::types::Role::System);
        assert_eq!(api[0].content.as_ref().and_then(Content::as_text), Some("You are helpful."));
    }

    #[test]
    fn restore_leaves_system_suffix_in_place() {
        let mut h = MessageHistory::new("You are helpful.".into());
        h.set_system_suffix(Some("Reply briefly.".into()));
        let before_suffix_tokens = h.estimated_tokens();
        h.restore(vec![Message::user("hi".into())]);
        let api = h.to_api_messages();
        assert_eq!(
            api[0].content.as_ref().and_then(Content::as_text),
            Some("You are helpful.\n\nReply briefly.")
        );
        // suffix contribution still reflected in the token count
        let mut baseline = MessageHistory::new("You are helpful.".into());
        baseline.set_system_suffix(Some("Reply briefly.".into()));
        assert!(before_suffix_tokens > 0);
        assert!(h.estimated_tokens() >= baseline.estimated_tokens());
    }

    #[test]
    fn restore_empty_vector_leaves_history_empty_with_system_prompt() {
        let mut h = MessageHistory::new("You are helpful.".into());
        h.push(Message::user("hi".into()));
        h.restore(Vec::new());
        assert!(h.is_empty());
        assert_eq!(h.len(), 0);
        let api = h.to_api_messages();
        assert_eq!(api.len(), 1);
        assert_eq!(api[0].content.as_ref().and_then(Content::as_text), Some("You are helpful."));
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
