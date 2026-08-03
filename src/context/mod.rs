// ── Thinking Store (T12) ──

use std::time::Instant;

use tracing::{debug, info};

/// A single thinking block extracted from the model's reasoning.
#[derive(Debug, Clone)]
pub struct ThinkingBlock {
    pub turn: usize,
    pub content: String,
    pub token_count: usize,
    pub relevance_score: f32,
    pub timestamp: Instant,
}

/// Accumulates thinking blocks across turns with relevance decay.
pub struct ThinkingStore {
    blocks: Vec<ThinkingBlock>,
    decay_threshold: f32,
}

impl ThinkingStore {
    pub fn new() -> Self {
        Self {
            blocks: Vec::new(),
            decay_threshold: 0.2,
        }
    }

    /// Add a thinking block.
    pub fn push(&mut self, turn: usize, content: String) {
        let token_count = content.chars().count() / 4;
        self.blocks.push(ThinkingBlock {
            turn,
            content,
            token_count,
            relevance_score: 1.0,
            timestamp: Instant::now(),
        });
        debug!("thinking: stored block for turn {turn}, {token_count} tokens");
    }

    /// Apply decay to all blocks and prune those below threshold.
    pub fn decay_all(&mut self) {
        let before = self.blocks.len();
        for block in &mut self.blocks {
            block.relevance_score *= 0.85;
        }
        self.blocks
            .retain(|b| b.relevance_score >= self.decay_threshold);
        let pruned = before - self.blocks.len();
        if pruned > 0 {
            info!(
                "thinking: pruned {pruned} blocks, {} remain",
                self.blocks.len()
            );
        }
    }

    /// Return all active thinking blocks.
    pub fn active_blocks(&self) -> &[ThinkingBlock] {
        &self.blocks
    }

    /// Total token count of stored thinking.
    pub fn total_tokens(&self) -> usize {
        self.blocks.iter().map(|b| b.token_count).sum()
    }
}

/// Parse `<think>...</think>` markers from content.
/// Returns (visible_content, extracted_thinking_blocks).
pub fn parse_thinking_tags(content: &str) -> (String, Vec<ThinkingBlock>) {
    let mut blocks = Vec::new();
    let mut visible = String::with_capacity(content.len());
    let mut remaining = content;

    loop {
        match remaining.find("<think>") {
            Some(start) => {
                visible.push_str(&remaining[..start]);
                let after_tag = &remaining[start + 7..];
                match after_tag.find("</think>") {
                    Some(end) => {
                        let thinking = after_tag[..end].to_string();
                        blocks.push(ThinkingBlock {
                            turn: 0,
                            content: thinking,
                            token_count: 0,
                            relevance_score: 1.0,
                            timestamp: Instant::now(),
                        });
                        remaining = &after_tag[end + 8..];
                    }
                    None => {
                        // No closing tag — treat rest as visible
                        visible.push_str(&remaining[start..]);
                        break;
                    }
                }
            }
            None => {
                visible.push_str(remaining);
                break;
            }
        }
    }

    (visible, blocks)
}

// ── Context Pruning (T13) ──

use crate::api::types::Message;

/// Scored message for context pruning.
#[derive(Debug, Clone)]
pub struct ScoredMessage {
    pub message: Message,
    pub relevance: f32,
    pub turn: usize,
    pub token_count: usize,
}

/// Prunes conversation context to stay within a token budget.
pub struct ContextPruner {
    target_tokens: usize,
    messages: Vec<ScoredMessage>,
}

impl ContextPruner {
    pub fn new(target_tokens: usize) -> Self {
        Self {
            target_tokens,
            messages: Vec::new(),
        }
    }

    /// Add a message with initial scoring.
    pub fn push(&mut self, turn: usize, message: Message) {
        let mut score = 1.0;

        // Tool errors: bonus
        if let Some(ref content) = message.content {
            if content.contains("error") || content.contains("Error") {
                score += 0.2;
            }
            // File paths: bonus
            if content.contains('/') || content.contains(".rs") || content.contains(".md") {
                score += 0.1;
            }
        }

        let token_count = message
            .content
            .as_ref()
            .map(|c| c.chars().count() / 4)
            .unwrap_or(0);

        self.messages.push(ScoredMessage {
            message,
            relevance: score,
            turn,
            token_count,
        });
    }

    /// Apply age-based scoring decay and “gradual forgetting.”
    pub fn score_all(&mut self, current_turn: usize) {
        for msg in &mut self.messages {
            let age = current_turn.saturating_sub(msg.turn);
            // Base decay: 0.9 per turn old
            msg.relevance *= 0.9_f32.powi(age as i32);
            // Gradual forgetting after 10 turns
            if age > 10 {
                msg.relevance -= 0.05 * (age - 10) as f32;
            }
        }
    }

    /// Prune lowest-scored messages until under target tokens.
    /// Never prunes system prompt or current turn.
    pub fn prune(&mut self, current_turn: usize) -> Vec<usize> {
        let total: usize = self.messages.iter().map(|m| m.token_count).sum();
        if total <= self.target_tokens {
            return Vec::new();
        }

        // Build list of (index, relevance) for eligible messages
        let mut candidates: Vec<(usize, f32)> = self
            .messages
            .iter()
            .enumerate()
            .filter(|(_, m)| m.turn != current_turn && m.turn != 0) // never prune system or current
            .map(|(i, m)| (i, m.relevance))
            .collect();

        // Sort by relevance (lowest first)
        candidates.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

        let mut freed = 0;
        let target = self.target_tokens;
        let mut to_remove: Vec<usize> = Vec::new();

        for (idx, _) in &candidates {
            if total.saturating_sub(freed) <= target {
                break;
            }
            freed += self.messages[*idx].token_count;
            to_remove.push(*idx);
        }

        // Remove in reverse order to preserve indices
        to_remove.sort_unstable_by(|a, b| b.cmp(a));
        for idx in &to_remove {
            self.messages.remove(*idx);
        }

        to_remove
    }

    /// Get remaining messages for API.
    pub fn to_messages(&self) -> Vec<Message> {
        self.messages.iter().map(|m| m.message.clone()).collect()
    }

    /// Current token count.
    pub fn current_tokens(&self) -> usize {
        self.messages.iter().map(|m| m.token_count).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Thinking tests ──

    #[test]
    fn parse_thinking_extracts_markers() {
        let input = "Hello <think>This is private reasoning</think> world";
        let (visible, blocks) = parse_thinking_tags(input);
        assert_eq!(visible.trim(), "Hello  world");
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].content.contains("private reasoning"));
    }

    #[test]
    fn parse_thinking_handles_malformed() {
        let input = "No closing <think> tag";
        let (visible, blocks) = parse_thinking_tags(input);
        assert_eq!(visible, input); // unchanged
        assert!(blocks.is_empty());
    }

    #[test]
    fn decay_reduces_scores_and_prunes() {
        let mut store = ThinkingStore::new();
        store.push(1, "thinking block with some reasoning content".into());
        store.push(2, "another block of thoughts".into());
        assert_eq!(store.active_blocks().len(), 2);

        // Decay many times
        for _ in 0..20 {
            store.decay_all();
        }
        // All should be pruned
        assert_eq!(store.active_blocks().len(), 0);
    }

    // ── Pruning tests ──

    use crate::api::types::Role;

    #[test]
    fn messages_scored_by_age() {
        let mut pruner = ContextPruner::new(1000);
        pruner.push(1, Message::user("hello".into()));
        pruner.push(2, Message::user("world".into()));
        pruner.score_all(5);
        // Older messages have lower relevance
        assert!(pruner.messages[0].relevance < pruner.messages[1].relevance);
    }

    #[test]
    fn pruning_removes_lowest_scored_first() {
        let mut pruner = ContextPruner::new(50); // very small target
        for i in 0..10 {
            pruner.push(i, Message::user(format!("msg {i} with some padding text")));
        }
        pruner.score_all(10);
        let removed = pruner.prune(10);
        assert!(!removed.is_empty());
        assert!(pruner.current_tokens() <= 50);
    }

    #[test]
    fn below_target_not_pruned() {
        let mut pruner = ContextPruner::new(10000);
        pruner.push(1, Message::user("short".into()));
        pruner.score_all(1);
        let removed = pruner.prune(1);
        assert!(removed.is_empty());
    }
}
