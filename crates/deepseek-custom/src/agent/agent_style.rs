use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use tracing::{debug, warn};

use crate::api::client::ApiClient;
use crate::api::types::{ChatRequest, Content, Message, Role};
use crate::effort::Effort;

/// Result of a critique-and-revise run: the final text and the number of
/// revision attempts it took (0 if the original text already passed).
pub struct StyleRevision {
    pub text: String,
    pub attempts: u32,
}

/// Replies shorter than this skip the plain-language grade check, since a
/// grade score on a one-word or one-sentence reply is just noise.
pub(crate) const MIN_PLAIN_LANGUAGE_LENGTH: usize = 100;

/// Shared style-gate state that can be moved out of AgentLoop without
/// holding a reference to the whole struct.
pub(crate) struct StyleState {
    pub plain_language_flag: Arc<AtomicBool>,
    pub target_grade_flag: Arc<AtomicU8>,
    pub grade_tolerance: f32,
    pub max_revise_attempts: u32,
}

impl StyleState {
    fn target_grade(&self) -> f32 {
        f32::from(self.target_grade_flag.load(Ordering::SeqCst))
    }

    /// Returns true when the reply needs revision to meet the
    /// plain-language target.
    pub fn needs_revision(&self, text: &str) -> bool {
        if !self.plain_language_flag.load(Ordering::SeqCst) {
            return false;
        }
        if text.len() < MIN_PLAIN_LANGUAGE_LENGTH {
            return false;
        }
        let grade = crate::style::flesch_kincaid_grade(text);
        let threshold = self.target_grade() + self.grade_tolerance;
        debug!(
            grade,
            target = self.target_grade(),
            tolerance = self.grade_tolerance,
            text_len = text.len(),
            "plain-language gate: grade {grade} vs threshold {threshold}"
        );
        grade > threshold
    }

    /// Revise a reply until it passes the plain-language gate, or until
    /// the attempt limit is reached. Returns the final text and the
    /// number of attempts taken.
    pub async fn revise(
        &self,
        client: &ApiClient,
        model: &str,
        max_tokens: u32,
        text: &str,
    ) -> StyleRevision {
        let max_attempts = self.max_revise_attempts;
        let mut current = text.to_string();
        let mut attempts = 0u32;

        let rubric = "Rewrite the following reply in plain language. \
            Use short sentences, common words, and active voice. \
            Cut padding, jargon, and passive constructions. \
            Preserve every technical fact, name, path, and code \
            reference exactly. Return only the rewritten reply, no \
            preamble or commentary.";

        let target_grade = self.target_grade();
        let tolerance = self.grade_tolerance;

        while attempts < max_attempts {
            let grade = crate::style::flesch_kincaid_grade(&current);
            let threshold = target_grade + tolerance;
            if grade <= threshold {
                debug!(
                    grade,
                    attempts,
                    "plain-language revise: grade {grade} within \
                     tolerance {threshold}, stopping"
                );
                break;
            }

            debug!(
                grade,
                attempt = attempts + 1,
                max_attempts,
                "plain-language revise: grade {grade} above \
                 threshold {threshold}, revising"
            );

            let request = ChatRequest {
                model: model.to_string(),
                messages: vec![
                    Message {
                        role: Role::System,
                        content: Some(Content::text(rubric)),
                        tool_calls: None,
                        tool_call_id: None,
                        reasoning_content: None,
                    },
                    Message {
                        role: Role::User,
                        content: Some(Content::text(current.clone())),
                        tool_calls: None,
                        tool_call_id: None,
                        reasoning_content: None,
                    },
                ],
                tools: None,
                tool_choice: None,
                stream: false,
                temperature: Some(0.3),
                max_tokens: Some(max_tokens),
                thinking: None,
                thinking_mode: None,
                reasoning_effort: None,
                effort: Some(Effort::None),
            };

            match client.chat(&request).await {
                Ok(response) => {
                    let revised = response
                        .choices
                        .first()
                        .and_then(|c| c.message.content.as_ref())
                        .and_then(|content| content.as_text())
                        .unwrap_or_default()
                        .to_string();
                    if revised.is_empty() {
                        warn!(
                            "plain-language revise: empty response \
                             from critic, stopping"
                        );
                        break;
                    }
                    current = revised;
                }
                Err(e) => {
                    warn!(
                        "plain-language revise: API error on \
                         attempt {}: {e}",
                        attempts + 1
                    );
                    break;
                }
            }
            attempts += 1;
        }

        StyleRevision {
            text: current,
            attempts,
        }
    }
}
