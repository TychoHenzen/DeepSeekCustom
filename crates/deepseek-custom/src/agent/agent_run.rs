use std::ops::ControlFlow;

use tracing::{debug, info, warn};

use crate::api::types::{Content, ImageAttachment, Message, Role, ToolCall, Usage};
use crate::error::Result;

use super::agent_helpers::{
    StreamCollection, assistant_with_tools, build_user_content, filter_valid_tool_calls,
};
use super::agent_loop::AgentLoop;
use super::events::StreamEvent;
use super::history::MessageHistory;
use super::prompt::SystemPromptBuilder;

impl AgentLoop {
    /// Run the agent loop for a single user message.
    pub async fn run(&mut self, user_input: &str) -> Result<Vec<String>> {
        self.run_with_image(user_input, None).await
    }

    /// Run the agent loop for a single user message, with an optional
    /// image attachment.
    pub async fn run_with_image(
        &mut self,
        user_input: &str,
        image: Option<&ImageAttachment>,
    ) -> Result<Vec<String>> {
        let result = self.run_turn(user_input, image).await;
        if let Some(registry) = self.subagent_registry.clone() {
            registry.close_all().await;
        }
        result
    }

    /// The turn body. `run_with_image` wraps this with subagent cleanup.
    pub(crate) async fn run_turn(
        &mut self,
        user_input: &str,
        image: Option<&ImageAttachment>,
    ) -> Result<Vec<String>> {
        self.prepare_turn(user_input, image);

        let mut assistant_texts: Vec<String> = Vec::new();

        for turn in 0..self.config.max_turns {
            debug!("turn {}/{}", turn + 1, self.config.max_turns);

            self.maybe_prune_context().await;

            let request = self.build_chat_request();
            let mut rx = self.client.chat_stream(&request);

            let collected = self.collect_stream(&mut rx).await;
            if collected.interrupted {
                self.handle_interrupted(&collected, &mut assistant_texts);
                break;
            }

            self.check_output_cap(&collected.finish_reason);

            let valid_tool_calls = filter_valid_tool_calls(&collected.tool_calls);

            if !valid_tool_calls.is_empty() {
                if self
                    .dispatch_tool_calls(turn, &collected, &valid_tool_calls)
                    .await
                    .is_break()
                {
                    return Ok(assistant_texts);
                }
                continue;
            }

            // Text-only response
            self.complete_text_turn(turn, &collected, &mut assistant_texts)
                .await;
            break;
        }

        if assistant_texts.is_empty() {
            warn!("max turns reached without final text response");
        }

        Ok(assistant_texts)
    }

    fn prepare_turn(&mut self, user_input: &str, image: Option<&ImageAttachment>) {
        self.sync_dynamic_config();
        let provider = self.client.provider();
        let built = build_user_content(provider, user_input, image);
        if let Some(message) = built.notice {
            self.send_event(StreamEvent::Error { message });
        }
        self.push_user_message(built.content);
    }

    pub(crate) fn check_output_cap(&self, finish_reason: &str) {
        if finish_reason == "length" {
            warn!(
                max_tokens = self.config.max_tokens,
                "reply hit the output cap and was cut off"
            );
            self.send_event(StreamEvent::Error {
                message: format!(
                    "The reply hit the {} token output cap and was \
                     cut off. Raise max_tokens in settings.json.",
                    self.config.max_tokens
                ),
            });
        }
    }

    fn handle_interrupted(&self, collected: &StreamCollection, texts: &mut Vec<String>) {
        if !collected.text.is_empty() {
            texts.push(collected.text.clone());
        }
        self.send_event(StreamEvent::Interrupted {
            message: "Interrupted by user (Escape)".into(),
        });
    }

    /// Build the chat request from current config and history.
    pub(crate) fn build_chat_request(&self) -> crate::api::types::ChatRequest {
        let tools = self.tools.to_api_definitions();
        let messages = self.history.to_api_messages();
        let effort = self.config.effort;
        info!(effort = ?effort, "building API request");

        crate::api::types::ChatRequest {
            model: self.config.model.clone(),
            messages,
            tools: (!tools.is_empty()).then_some(tools),
            tool_choice: None,
            stream: true,
            temperature: Some(0.7),
            max_tokens: Some(self.config.max_tokens),
            thinking: None,
            thinking_mode: None,
            reasoning_effort: None,
            response_format: None,
            effort: Some(effort),
        }
    }

    /// Push a user-role message onto history.
    pub(crate) fn push_user_message(&mut self, content: Content) {
        self.history.push(Message {
            role: Role::User,
            content: Some(content),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        });
    }

    /// Build assistant message with tool calls from the collected stream
    /// and delegate to `run_tool_calls`.
    pub(crate) async fn dispatch_tool_calls(
        &mut self,
        turn: u32,
        collected: &StreamCollection,
        tool_calls: &[ToolCall],
    ) -> ControlFlow<()> {
        self.history.push(assistant_with_tools(
            &collected.text,
            &collected.reasoning,
            tool_calls,
        ));
        self.run_tool_calls(turn, tool_calls, &collected.finish_reason, &collected.usage)
            .await
    }

    /// Run the plain-language gate on a text-only reply,
    /// push the final assistant message, and send the terminal events.
    pub(crate) async fn complete_text_turn(
        &mut self,
        turn: u32,
        collected: &StreamCollection,
        assistant_texts: &mut Vec<String>,
    ) {
        let stream_text = collected.text.clone();
        let final_text = self.finalize_text_reply(turn, stream_text).await;
        if !final_text.is_empty() {
            assistant_texts.push(final_text.clone());
        }

        let reasoning = (!collected.reasoning.is_empty()).then(|| collected.reasoning.clone());

        self.history.push(Message {
            role: Role::Assistant,
            content: Some(Content::text(final_text)),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: reasoning,
        });

        self.send_snapshot_and_turn_end(turn, &collected.finish_reason, &collected.usage);
    }

    /// Run the plain-language gate on a text-only reply.
    pub(crate) async fn finalize_text_reply(&mut self, turn: u32, stream_text: String) -> String {
        let mut final_text = stream_text;
        if !self.style_state.needs_revision(&final_text) {
            return final_text;
        }

        let revision = self
            .style_state
            .revise(
                &self.client,
                &self.config.model,
                self.config.max_tokens,
                &final_text,
            )
            .await;
        if revision.text != final_text {
            let orig_grade = crate::style::flesch_kincaid_grade(&final_text);
            let rev_grade = crate::style::flesch_kincaid_grade(&revision.text);
            info!(
                attempts = revision.attempts,
                original_grade = orig_grade,
                revised_grade = rev_grade,
                "plain-language gate: reply revised"
            );
            self.send_event(StreamEvent::Info {
                message: format!(
                    "Reply revised for plain language \
                     ({} attempt(s))",
                    revision.attempts
                ),
            });
            self.send_event(StreamEvent::Text {
                turn: turn + 1,
                text: revision.text.clone(),
            });
            final_text = revision.text;
        }
        final_text
    }

    pub(crate) fn send_snapshot_and_turn_end(
        &self,
        turn: u32,
        finish_reason: &str,
        stream_usage: &Option<Usage>,
    ) {
        self.send_event(StreamEvent::ConversationSnapshot {
            messages: self.history.iter().cloned().collect(),
            claude_session_id: None,
        });
        let hit = stream_usage
            .as_ref()
            .map(|u| u.prompt_cache_hit_tokens)
            .unwrap_or(0);
        let miss = stream_usage
            .as_ref()
            .map(|u| u.prompt_cache_miss_tokens)
            .unwrap_or(0);
        self.send_event(StreamEvent::TurnEnd {
            turn: turn + 1,
            finish_reason: finish_reason.to_string(),
            total_tokens: self.history.estimated_tokens(),
            prompt_cache_hit_tokens: hit,
            prompt_cache_miss_tokens: miss,
        });
    }

    pub fn rebuild_system_prompt(
        &mut self,
        memory_fragment: Option<&str>,
        skills_fragment: Option<&str>,
    ) {
        let builder = SystemPromptBuilder::new();
        let tools = self.tools.to_api_definitions();
        let prompt = builder.build(memory_fragment, skills_fragment, &tools);
        self.history = MessageHistory::new(prompt);
    }
}
