use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::api::client::ApiClient;
use crate::api::types::{ChatRequest, Content, ImageAttachment, Message, Role, ToolCall};
use crate::effort::Effort;
use crate::error::{HarnessError, Result};
use crate::tools::{ToolOutput, ToolRegistry};

use super::agent_helpers::{
    StreamCollection, assistant_with_tools, build_user_content, filter_valid_tool_calls,
    merge_tool_call,
};
use super::agent_style::StyleState;
use super::agent_types::{AgentConfig, DEFAULT_CONTEXT_BUDGET, DEFAULT_TARGET_GRADE, grade_to_u8};
use super::events::{RoutedEvent, StreamEvent};
use super::history::{MessageHistory, PruneReport};
use super::prompt::{SystemPromptBuilder, voice_mode_instructions};

/// Core agent loop: user input -> API call -> tool execution -> repeat.
pub struct AgentLoop {
    client: ApiClient,
    tools: ToolRegistry,
    history: MessageHistory,
    config: AgentConfig,
    tx_events: Option<mpsc::UnboundedSender<RoutedEvent>>,
    interrupt_flag: Arc<AtomicBool>,
    effort_flag: Arc<AtomicU8>,
    model_name: Arc<Mutex<String>>,
    voice_mode_flag: Arc<AtomicBool>,
    context_budget: Arc<AtomicUsize>,
    repeat_interrupt_flag: Arc<AtomicBool>,
    subagent_registry: Option<Arc<crate::backend::registry::SubagentRegistry>>,
    working_dir: Option<Arc<Mutex<PathBuf>>>,
    style_state: StyleState,
    style_critic_backend: Option<String>,
}

impl AgentLoop {
    pub fn new(
        client: ApiClient,
        tools: ToolRegistry,
        system_prompt: String,
        config: AgentConfig,
        interrupt_flag: Arc<AtomicBool>,
    ) -> Self {
        let effort = config.effort;
        let model = config.model.clone();
        let style_plain = Arc::new(AtomicBool::new(false));
        let style_grade = Arc::new(AtomicU8::new(DEFAULT_TARGET_GRADE));
        Self {
            client,
            tools,
            history: MessageHistory::new(system_prompt),
            config,
            tx_events: None,
            interrupt_flag,
            effort_flag: Arc::new(AtomicU8::new(effort.to_u8())),
            model_name: Arc::new(Mutex::new(model)),
            voice_mode_flag: Arc::new(AtomicBool::new(false)),
            context_budget: Arc::new(AtomicUsize::new(DEFAULT_CONTEXT_BUDGET)),
            repeat_interrupt_flag: Arc::new(AtomicBool::new(false)),
            subagent_registry: None,
            working_dir: None,
            style_state: StyleState {
                plain_language_flag: style_plain,
                target_grade_flag: style_grade,
                grade_tolerance: 2.0,
                max_revise_attempts: 2,
            },
            style_critic_backend: None,
        }
    }

    pub fn set_event_sender(&mut self, tx: mpsc::UnboundedSender<RoutedEvent>) {
        self.tx_events = Some(tx);
    }

    pub fn set_subagent_registry(
        &mut self,
        registry: Arc<crate::backend::registry::SubagentRegistry>,
    ) {
        self.subagent_registry = Some(registry);
    }

    pub fn set_working_dir(&mut self, working_dir: Arc<Mutex<PathBuf>>) {
        self.working_dir = Some(working_dir);
    }

    pub fn set_style_config(
        &mut self,
        plain_language_enabled: bool,
        target_grade: f32,
        grade_tolerance: f32,
        max_revise_attempts: u32,
        critic_backend: Option<String>,
    ) {
        self.style_state
            .plain_language_flag
            .store(plain_language_enabled, Ordering::SeqCst);
        self.style_state
            .target_grade_flag
            .store(grade_to_u8(target_grade), Ordering::SeqCst);
        self.style_state.grade_tolerance = grade_tolerance;
        self.style_state.max_revise_attempts = max_revise_attempts;
        self.style_critic_backend = critic_backend;
    }

    pub fn style_plain_language_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.style_state.plain_language_flag)
    }

    pub fn style_target_grade_flag(&self) -> Arc<AtomicU8> {
        Arc::clone(&self.style_state.target_grade_flag)
    }

    pub fn set_effort_flag(&mut self, effort_flag: Arc<AtomicU8>) {
        self.effort_flag = effort_flag;
    }

    pub fn adopt_flags(&mut self, flags: &crate::backend::SharedFlags) {
        self.interrupt_flag = Arc::clone(&flags.interrupt);
        self.model_name = Arc::clone(&flags.model);
        self.voice_mode_flag = Arc::clone(&flags.voice_mode);
        self.context_budget = Arc::clone(&flags.context_budget);
        self.repeat_interrupt_flag = Arc::clone(&flags.repeat_interrupt);
        self.style_state.plain_language_flag = Arc::clone(&flags.style_plain_language);
        self.style_state.target_grade_flag = Arc::clone(&flags.style_target_grade);
    }

    #[cfg(feature = "test-support")]
    pub fn subagent_registry_for_test(
        &self,
    ) -> Option<Arc<crate::backend::registry::SubagentRegistry>> {
        self.subagent_registry.clone()
    }

    pub fn interrupt_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.interrupt_flag)
    }

    pub fn effort_flag(&self) -> Arc<AtomicU8> {
        Arc::clone(&self.effort_flag)
    }

    pub fn model_flag(&self) -> Arc<Mutex<String>> {
        Arc::clone(&self.model_name)
    }

    pub fn voice_mode_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.voice_mode_flag)
    }

    pub fn context_budget_flag(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.context_budget)
    }

    pub fn repeat_interrupt_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.repeat_interrupt_flag)
    }

    #[cfg(feature = "test-support")]
    pub fn tool_names(&self) -> Vec<String> {
        self.tools
            .list()
            .iter()
            .map(|t| t.name().to_string())
            .collect()
    }

    pub fn clear_history(&mut self) {
        self.history = MessageHistory::new(self.history.system_prompt().to_string());
    }

    pub fn restore_history(&mut self, messages: Vec<Message>) {
        self.history.restore(messages);
    }

    fn sync_dynamic_config(&mut self) {
        self.config.effort = Effort::load(&self.effort_flag);
        if let Ok(model) = self.model_name.lock() {
            self.config.model.clone_from(&*model);
        }
        if self.voice_mode_flag.load(Ordering::SeqCst) {
            self.history
                .set_system_suffix(Some(voice_mode_instructions().to_string()));
        } else {
            self.history.set_system_suffix(None);
        }
        if let Some(working_dir) = &self.working_dir
            && let Ok(dir) = working_dir.lock()
        {
            self.history
                .set_working_dir(Some(dir.display().to_string()));
        }
    }

    #[cfg(feature = "test-support")]
    pub fn sync_dynamic_config_for_test(&mut self) {
        self.sync_dynamic_config();
    }

    fn apply_prune(&mut self, scores: Option<&[f32]>) -> PruneReport {
        let budget = self.context_budget.load(Ordering::SeqCst);
        let report = self
            .history
            .prune_to_budget(super::agent_helpers::context_low_water(budget), scores);
        info!(
            tokens_before = report.tokens_before,
            tokens_after = report.tokens_after,
            images_elided = report.images_elided,
            tool_bodies_elided = report.tool_bodies_elided,
            groups_collapsed = report.groups_collapsed,
            groups_dropped = report.groups_dropped,
            "context pruned"
        );
        report
    }

    #[cfg(feature = "test-support")]
    pub fn apply_prune_for_test(&mut self, scores: Option<&[f32]>) -> PruneReport {
        self.apply_prune(scores)
    }

    async fn maybe_prune_context(&mut self) {
        let budget = self.context_budget.load(Ordering::SeqCst);
        if self.history.estimated_tokens() <= budget {
            return;
        }

        let messages: Vec<Message> = self.history.iter().cloned().collect();
        let scores =
            crate::context::relevance::score_messages(&self.client, &messages, &self.config.model)
                .await;
        if scores.is_none() {
            warn!(
                "context pruning: relevance scoring failed, \
                 falling back to oldest-first order"
            );
        }
        self.apply_prune(scores.as_deref());
    }

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
    async fn run_turn(
        &mut self,
        user_input: &str,
        image: Option<&ImageAttachment>,
    ) -> Result<Vec<String>> {
        self.sync_dynamic_config();

        let built = build_user_content(self.client.provider(), user_input, image);
        if let Some(message) = built.notice {
            self.send_event(StreamEvent::Error { message });
        }

        self.push_user_message(built.content);

        let mut assistant_texts: Vec<String> = Vec::new();

        for turn in 0..self.config.max_turns {
            debug!("turn {}/{}", turn + 1, self.config.max_turns);

            self.maybe_prune_context().await;

            let request = self.build_chat_request();
            let mut rx = self.client.chat_stream(&request);

            let collected = self.collect_stream(&mut rx).await;
            if collected.interrupted {
                if !collected.text.is_empty() {
                    assistant_texts.push(collected.text.clone());
                }
                self.send_event(StreamEvent::Interrupted {
                    message: "Interrupted by user (Escape)".into(),
                });
                break;
            }

            self.check_output_cap(&collected.finish_reason);

            let valid_tool_calls = filter_valid_tool_calls(&collected.tool_calls);

            if !valid_tool_calls.is_empty() {
                self.history.push(assistant_with_tools(
                    &collected.text,
                    &collected.reasoning,
                    &valid_tool_calls,
                ));

                if self
                    .run_tool_calls(
                        turn,
                        &valid_tool_calls,
                        &collected.finish_reason,
                        &collected.usage,
                    )
                    .await
                    .is_break()
                {
                    return Ok(assistant_texts);
                }

                continue;
            }

            // Text-only response
            let final_text = self.finalize_text_reply(turn, collected.text).await;
            if !final_text.is_empty() {
                assistant_texts.push(final_text.clone());
            }

            self.history.push(Message {
                role: Role::Assistant,
                content: Some(Content::text(final_text)),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: if collected.reasoning.is_empty() {
                    None
                } else {
                    Some(collected.reasoning.clone())
                },
            });

            self.send_snapshot_and_turn_end(turn, &collected.finish_reason, &collected.usage);
            break;
        }

        if assistant_texts.is_empty() {
            warn!("max turns reached without final text response");
        }

        Ok(assistant_texts)
    }

    fn check_output_cap(&self, finish_reason: &str) {
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

    /// Build the chat request from current config and history.
    fn build_chat_request(&self) -> ChatRequest {
        let tools = self.tools.to_api_definitions();
        let messages = self.history.to_api_messages();
        let effort = self.config.effort;
        info!(effort = ?effort, "building API request");

        ChatRequest {
            model: self.config.model.clone(),
            messages,
            tools: if tools.is_empty() { None } else { Some(tools) },
            tool_choice: None,
            stream: true,
            temperature: Some(0.7),
            max_tokens: Some(self.config.max_tokens),
            thinking: None,
            thinking_mode: None,
            reasoning_effort: None,
            effort: Some(effort),
        }
    }

    /// Collect a stream of SSE chunks into a `StreamCollection`.
    async fn collect_stream(
        &mut self,
        rx: &mut mpsc::UnboundedReceiver<
            std::result::Result<crate::api::types::StreamChunk, crate::error::HarnessError>,
        >,
    ) -> StreamCollection {
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut finish_reason = String::new();
        let mut usage: Option<crate::api::types::Usage> = None;
        let mut interrupted = false;

        while let Some(chunk_result) = rx.recv().await {
            if self.interrupt_flag.load(Ordering::SeqCst) {
                debug!("interrupt detected during stream receive");
                interrupted = true;
                break;
            }
            match chunk_result {
                Ok(chunk) => {
                    self.process_chunk(
                        &chunk,
                        &mut text,
                        &mut reasoning,
                        &mut tool_calls,
                        &mut finish_reason,
                    );
                    if chunk.usage.is_some() {
                        usage = chunk.usage;
                    }
                }
                Err(e) => {
                    error!("stream error: {e}");
                    self.send_event(StreamEvent::Error {
                        message: format!("{e}"),
                    });
                    return StreamCollection {
                        text,
                        reasoning,
                        tool_calls,
                        finish_reason,
                        usage,
                        interrupted: true,
                    };
                }
            }
        }

        debug!(
            "stream complete: text_len={}, reasoning_len={}, \
             tool_calls={}, finish={}",
            text.len(),
            reasoning.len(),
            tool_calls.len(),
            finish_reason,
        );

        StreamCollection {
            text,
            reasoning,
            tool_calls,
            finish_reason,
            usage,
            interrupted,
        }
    }

    /// Process one SSE chunk into the accumulators.
    fn process_chunk(
        &self,
        chunk: &crate::api::types::StreamChunk,
        text: &mut String,
        reasoning: &mut String,
        tool_calls: &mut Vec<ToolCall>,
        finish_reason: &mut String,
    ) {
        if let Some(ref choices) = chunk.choices {
            for choice in choices {
                if let Some(ref content) = choice.delta.content {
                    text.push_str(content);
                    self.send_event(StreamEvent::Text {
                        turn: 1,
                        text: content.clone(),
                    });
                }
                if let Some(ref r) = choice.delta.reasoning_content {
                    reasoning.push_str(r);
                    self.send_event(StreamEvent::Reasoning {
                        turn: 1,
                        text: r.clone(),
                    });
                }
                if let Some(ref tcs) = choice.delta.tool_calls {
                    for tc in tcs {
                        merge_tool_call(tool_calls, tc);
                    }
                }
                if let Some(ref fr) = choice.finish_reason {
                    finish_reason.clone_from(fr);
                }
            }
        }
    }

    /// Push a user-role message onto history.
    fn push_user_message(&mut self, content: Content) {
        self.history.push(Message {
            role: Role::User,
            content: Some(content),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        });
    }

    /// Run the plain-language gate on a text-only reply.
    async fn finalize_text_reply(&mut self, turn: u32, stream_text: String) -> String {
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
            info!(
                attempts = revision.attempts,
                original_grade = crate::style::flesch_kincaid_grade(&final_text),
                revised_grade = crate::style::flesch_kincaid_grade(&revision.text),
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

    /// Execute one batch of tool calls and handle any tool-returned images.
    async fn run_tool_calls(
        &mut self,
        turn: u32,
        tool_calls: &[ToolCall],
        finish_reason: &str,
        stream_usage: &Option<crate::api::types::Usage>,
    ) -> ControlFlow<()> {
        for tc in tool_calls {
            if self.interrupt_flag.load(Ordering::SeqCst) {
                info!("agent: user interrupted before tool execution");
                self.interrupt_flag.store(false, Ordering::SeqCst);
                self.send_event(StreamEvent::Interrupted {
                    message: "Interrupted by user (Escape)".into(),
                });
                return ControlFlow::Break(());
            }

            let func = tc.function.as_ref();
            let tool_name = func.and_then(|f| f.name.as_deref()).unwrap_or("unknown");
            let tool_args = func.and_then(|f| f.arguments.as_deref()).unwrap_or("{}");

            self.send_event(StreamEvent::ToolCallStart {
                turn: turn + 1,
                tool: tool_name.to_string(),
                args: tool_args.to_string(),
            });

            let result = self.execute_tool(tool_name, tool_args).await;

            self.send_event(StreamEvent::ToolCallEnd {
                turn: turn + 1,
                tool: tool_name.to_string(),
                output: result.content.clone(),
                is_error: result.is_error,
            });

            self.history.push(Message {
                role: Role::Tool,
                content: Some(Content::text(result.content)),
                tool_calls: None,
                tool_call_id: Some(tc.id.clone()),
                reasoning_content: None,
            });

            if let Some(image) = result.image {
                let built = build_user_content(
                    self.client.provider(),
                    "(image read by the tool call above)",
                    Some(&image),
                );
                if let Some(message) = built.notice {
                    self.send_event(StreamEvent::Error { message });
                }
                self.push_user_message(built.content);
            }
        }

        self.send_snapshot_and_turn_end(turn, finish_reason, stream_usage);
        ControlFlow::Continue(())
    }

    fn send_snapshot_and_turn_end(
        &self,
        turn: u32,
        finish_reason: &str,
        stream_usage: &Option<crate::api::types::Usage>,
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

    fn arg_parse_error(&self, name: &str, args: &str, e: &serde_json::Error) -> String {
        if e.classify() != serde_json::error::Category::Eof {
            return format!("Tool error: Invalid input: {e}");
        }
        format!(
            "Tool error: the arguments for '{name}' stop part way \
             through ({e}). The reply ran into the {} token output \
             cap while writing them, so the call never finished. It \
             carried {} characters. Retry with a smaller payload: \
             use `edit` to change part of a file rather than \
             `write` to replace all of it, or write the file in \
             several smaller calls. Raise `max_tokens` in \
             settings.json if the payload cannot be split.",
            self.config.max_tokens,
            args.len(),
        )
    }

    pub(crate) async fn execute_tool(&self, name: &str, args: &str) -> ToolOutput {
        let input: serde_json::Value = match serde_json::from_str(args) {
            Ok(v) => v,
            Err(e) => {
                warn!("execute_tool: failed to parse args for '{}': {}", name, e);
                return ToolOutput {
                    content: self.arg_parse_error(name, args, &e),
                    is_error: true,
                    image: None,
                };
            }
        };

        match self.tools.get(name) {
            Some(tool) => match tool.execute(input).await {
                Ok(output) => output,
                Err(HarnessError::SessionReset) => {
                    info!("session reset triggered by tool '{}'", name);
                    if let Some(registry) = self.subagent_registry.clone() {
                        registry.close_all().await;
                    }
                    self.send_event(StreamEvent::SessionReset);
                    ToolOutput {
                        content: "Session reset initiated.".into(),
                        is_error: false,
                        image: None,
                    }
                }
                Err(e) => ToolOutput {
                    content: format!("Tool error: {e}"),
                    is_error: true,
                    image: None,
                },
            },
            None => ToolOutput {
                content: format!(
                    "Unknown tool: {name}. Available tools: {}",
                    self.tool_name_list()
                ),
                is_error: true,
                image: None,
            },
        }
    }

    fn tool_name_list(&self) -> String {
        let mut names: Vec<String> = self
            .tools
            .list()
            .iter()
            .map(|tool| tool.name().to_string())
            .collect();
        names.sort();
        names.join(", ")
    }

    #[cfg(feature = "test-support")]
    pub async fn execute_tool_for_test(&self, name: &str, args: &str) -> ToolOutput {
        self.execute_tool(name, args).await
    }

    pub(crate) fn send_event(&self, event: StreamEvent) {
        if let Some(ref tx) = self.tx_events {
            let _ = tx.send(RoutedEvent::own(event));
        }
    }

    pub(crate) fn send_repeat_finished(&self, completed: u32, total: u32) {
        self.send_event(StreamEvent::RepeatFinished { completed, total });
    }

    pub fn history(&self) -> &MessageHistory {
        &self.history
    }

    #[cfg(feature = "test-support")]
    pub fn history_mut(&mut self) -> &mut MessageHistory {
        &mut self.history
    }

    #[cfg(feature = "test-support")]
    pub fn config(&self) -> &AgentConfig {
        &self.config
    }

    #[cfg(feature = "test-support")]
    pub fn maybe_check_plain_language_for_test(&self, text: &str) -> bool {
        self.style_state.needs_revision(text)
    }

    #[cfg(feature = "test-support")]
    pub async fn revise_for_plain_language_for_test(
        &self,
        text: &str,
    ) -> super::agent_style::StyleRevision {
        self.style_state
            .revise(
                &self.client,
                &self.config.model,
                self.config.max_tokens,
                text,
            )
            .await
    }

    pub fn reset(&mut self, new_system_prompt: String, new_user_prompt: String) {
        info!("agent: session reset");
        self.history = MessageHistory::new(new_system_prompt);
        self.history.push(Message {
            role: Role::User,
            content: Some(Content::text(new_user_prompt)),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        });
        self.send_event(StreamEvent::SessionReset);
    }

    pub fn rebuild_system_prompt(
        &mut self,
        memory_fragment: Option<&str>,
        skills_fragment: Option<&str>,
    ) {
        let builder = SystemPromptBuilder::new();
        let tools = self.tools.to_api_definitions();
        let new_prompt = builder.build(memory_fragment, skills_fragment, &tools);
        self.history = MessageHistory::new(new_prompt);
    }
}
