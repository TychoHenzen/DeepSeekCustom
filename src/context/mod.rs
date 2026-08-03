// ── Thinking Store (T12) ──

pub mod relevance;

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
                        // No closing tag - treat rest as visible
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
}
