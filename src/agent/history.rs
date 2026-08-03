use crate::api::types::Message;

/// Thread-safe conversation history with approximate token tracking.
pub struct MessageHistory {
    system_prompt: String,
    messages: Vec<Message>,
    token_count: usize,
}

impl MessageHistory {
    pub fn new(system_prompt: String) -> Self {
        let token_count = estimate_tokens(&system_prompt);
        Self {
            system_prompt,
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

    /// Clear all messages (keep system prompt).
    pub fn clear(&mut self) {
        self.messages.clear();
        self.token_count = estimate_tokens(&self.system_prompt);
    }

    /// Return messages in API-ready format (system prompt first).
    pub fn to_api_messages(&self) -> Vec<Message> {
        let mut out = Vec::with_capacity(self.messages.len() + 1);
        out.push(Message::system(self.system_prompt.clone()));
        out.extend(self.messages.clone());
        out
    }

    /// Approximate token count (system prompt + all messages).
    /// Uses rough heuristic: 1 token ≈ 4 characters for English text.
    pub fn estimated_tokens(&self) -> usize {
        self.token_count
    }
}

/// Estimate tokens from a string: ~4 chars per token.
fn estimate_tokens(s: &str) -> usize {
    s.chars().count().div_ceil(4)
}

fn estimate_message_tokens(msg: &Message) -> usize {
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
}
