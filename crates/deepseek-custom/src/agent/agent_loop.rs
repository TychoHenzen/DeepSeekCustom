use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::api::client::{ApiClient, Provider};
use crate::api::types::{
    ChatRequest, Content, ContentPart, ImageAttachment, Message, Role, ToolCall,
};
use crate::effort::Effort;
use crate::error::{HarnessError, Result};
use crate::tools::{ToolOutput, ToolRegistry};

use super::events::{RoutedEvent, StreamEvent};
use super::history::{MessageHistory, PruneReport};
use super::prompt::{SystemPromptBuilder, voice_mode_instructions};

/// Default token budget for the context pruning hysteresis oscillator.
/// History grows freely until it passes this high-water mark, then gets
/// pruned hard down to a third of it (see `context_low_water`).
pub const DEFAULT_CONTEXT_BUDGET: usize = 100_000;

/// Replies shorter than this skip the plain-language grade check, since a
/// grade score on a one-word or one-sentence reply is just noise. The
/// threshold is character count, not token count, because the text already
/// arrived before the check runs.
const MIN_PLAIN_LANGUAGE_LENGTH: usize = 100;

/// Default target Flesch-Kincaid grade for the plain-language gate,
/// matching `Settings::style_target_grade`.
pub const DEFAULT_TARGET_GRADE: u8 = 8;

/// Round a configured target grade onto the whole number the shared flag
/// carries. A grade below zero or past 30 is clamped rather than wrapped,
/// so a stray value in settings.json cannot turn into a nonsense target.
pub fn grade_to_u8(grade: f32) -> u8 {
    grade.round().clamp(0.0, 30.0) as u8
}

/// Configuration for the agent loop.
pub struct AgentConfig {
    pub max_turns: u32,
    pub model: String,
    pub effort: Effort,
    /// Cap on the tokens one API reply may produce, reasoning included.
    pub max_tokens: u32,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_turns: 100,
            model: "deepseek-v4-flash".into(),
            effort: Effort::None,
            max_tokens: 8192,
        }
    }
}

/// Result of a critique-and-revise run: the final text and the number of
/// revision attempts it took (0 if the original text already passed).
pub struct StyleRevision {
    pub text: String,
    pub attempts: u32,
}

/// Result of building a user's content for an API request: the `Content`
/// to send, and an optional notice to post in the transcript when the
/// image could not be carried on this provider.
pub struct BuiltUserContent {
    pub content: Content,
    pub notice: Option<String>,
}

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
    style_plain_language_flag: Arc<AtomicBool>,
    style_target_grade_flag: Arc<AtomicU8>,
    style_grade_tolerance: f32,
    style_max_revise_attempts: u32,
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
            style_plain_language_flag: Arc::new(AtomicBool::new(false)),
            style_target_grade_flag: Arc::new(AtomicU8::new(DEFAULT_TARGET_GRADE)),
            style_grade_tolerance: 2.0,
            style_max_revise_attempts: 2,
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
        self.style_plain_language_flag
            .store(plain_language_enabled, Ordering::SeqCst);
        self.style_target_grade_flag
            .store(grade_to_u8(target_grade), Ordering::SeqCst);
        self.style_grade_tolerance = grade_tolerance;
        self.style_max_revise_attempts = max_revise_attempts;
        self.style_critic_backend = critic_backend;
    }

    fn style_target_grade(&self) -> f32 {
        f32::from(self.style_target_grade_flag.load(Ordering::SeqCst))
    }

    pub fn style_plain_language_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.style_plain_language_flag)
    }

    pub fn style_target_grade_flag(&self) -> Arc<AtomicU8> {
        Arc::clone(&self.style_target_grade_flag)
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
        self.style_plain_language_flag = Arc::clone(&flags.style_plain_language);
        self.style_target_grade_flag = Arc::clone(&flags.style_target_grade);
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
            .prune_to_budget(context_low_water(budget), scores);
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
    /// Returns the final assistant text or an error.
    pub async fn run(&mut self, user_input: &str) -> Result<Vec<String>> {
        self.run_with_image(user_input, None).await
    }

    /// Run the agent loop for a single user message, with an optional
    /// image attachment. `run` delegates to this method with `None`.
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

        self.history.push(Message {
            role: Role::User,
            content: Some(built.content),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        });

        let mut assistant_texts: Vec<String> = Vec::new();

        for turn in 0..self.config.max_turns {
            debug!("turn {}/{}", turn + 1, self.config.max_turns);

            self.maybe_prune_context().await;

            let tools = self.tools.to_api_definitions();
            let messages = self.history.to_api_messages();

            let effort = self.config.effort;
            info!(effort = ?effort, "building API request");

            let request = ChatRequest {
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
            };

            let mut rx = self.client.chat_stream(&request);

            let mut stream_text = String::new();
            let mut stream_reasoning = String::new();
            let mut stream_tool_calls: Vec<ToolCall> = Vec::new();
            let mut finish_reason = String::new();
            let mut stream_usage: Option<crate::api::types::Usage> = None;

            while let Some(chunk_result) = rx.recv().await {
                if self.interrupt_flag.load(Ordering::SeqCst) {
                    debug!("interrupt detected during stream receive");
                    break;
                }
                match chunk_result {
                    Ok(chunk) => {
                        if let Some(ref choices) = chunk.choices {
                            for choice in choices {
                                if let Some(ref content) = choice.delta.content {
                                    stream_text.push_str(content);
                                    self.send_event(StreamEvent::Text {
                                        turn: turn + 1,
                                        text: content.clone(),
                                    });
                                }
                                if let Some(ref reasoning) = choice.delta.reasoning_content {
                                    stream_reasoning.push_str(reasoning);
                                    self.send_event(StreamEvent::Reasoning {
                                        turn: turn + 1,
                                        text: reasoning.clone(),
                                    });
                                }
                                if let Some(ref tcs) = choice.delta.tool_calls {
                                    for tc in tcs {
                                        merge_tool_call(&mut stream_tool_calls, tc);
                                    }
                                }
                                if let Some(ref fr) = choice.finish_reason {
                                    finish_reason = fr.clone();
                                }
                            }
                        }
                        if chunk.usage.is_some() {
                            stream_usage = chunk.usage;
                        }
                    }
                    Err(e) => {
                        error!("stream error: {e}");
                        self.send_event(StreamEvent::Error {
                            message: format!("{e}"),
                        });
                        return Err(e);
                    }
                }
            }

            // Check if stream was interrupted
            if self.interrupt_flag.swap(false, Ordering::SeqCst) {
                info!("agent: user interrupted stream");
                if !stream_text.is_empty() {
                    assistant_texts.push(stream_text.clone());
                }
                self.send_event(StreamEvent::Interrupted {
                    message: "Interrupted by user (Escape)".into(),
                });
                break;
            }

            debug!(
                "stream complete: text_len={}, reasoning_len={}, \
                 tool_calls={}, finish={}",
                stream_text.len(),
                stream_reasoning.len(),
                stream_tool_calls.len(),
                finish_reason,
            );

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

            // Filter out tool calls lacking a function name
            let valid_tool_calls: Vec<ToolCall> = stream_tool_calls
                .iter()
                .filter(|tc| tc.function.as_ref().and_then(|f| f.name.as_ref()).is_some())
                .cloned()
                .collect();
            let filtered_out = stream_tool_calls.len() - valid_tool_calls.len();
            if filtered_out > 0 {
                info!(
                    total = stream_tool_calls.len(),
                    valid = valid_tool_calls.len(),
                    "filtered out {filtered_out} nameless tool call(s)"
                );
            }

            if !valid_tool_calls.is_empty() {
                stream_tool_calls = valid_tool_calls;

                self.history.push(assistant_with_tools(
                    &stream_text,
                    &stream_reasoning,
                    &stream_tool_calls,
                ));

                // An interrupt inside the batch ends the whole turn, rather
                // than falling through to another API call with the user's
                // Escape already reported and consumed.
                if self
                    .run_tool_calls(turn, &stream_tool_calls, &finish_reason, &stream_usage)
                    .await
                    .is_break()
                {
                    return Ok(assistant_texts);
                }

                // Continue the turn loop for another API call
                continue;
            }

            // Text-only response
            let final_text = self.finalize_text_reply(turn, stream_text).await;
            if !final_text.is_empty() {
                assistant_texts.push(final_text.clone());
            }

            self.history.push(Message {
                role: Role::Assistant,
                content: Some(Content::text(final_text)),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: if stream_reasoning.is_empty() {
                    None
                } else {
                    Some(stream_reasoning.clone())
                },
            });

            self.send_snapshot_and_turn_end(turn, &finish_reason, &stream_usage);
            break;
        }

        if assistant_texts.is_empty() {
            warn!("max turns reached without final text response");
        }

        Ok(assistant_texts)
    }

    /// Run the plain-language gate on a text-only reply and emit the result.
    /// Returns the final text to record.
    async fn finalize_text_reply(&mut self, turn: u32, stream_text: String) -> String {
        let mut final_text = stream_text;
        if !self.maybe_check_plain_language(&final_text) {
            return final_text;
        }

        let revision = self.revise_for_plain_language(&final_text).await;
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
    ///
    /// Breaks when the user interrupted part way through the batch, which
    /// ends the caller's turn. Sending `Interrupted` and then carrying on
    /// into another API call would report a stop that never happened.
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
                self.history.push(Message {
                    role: Role::User,
                    content: Some(built.content),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                });
            }
        }

        self.send_snapshot_and_turn_end(turn, finish_reason, stream_usage);
        ControlFlow::Continue(())
    }

    /// Send the conversation snapshot, then `TurnEnd`.
    ///
    /// The snapshot is read off `self.history` here, at the moment it goes
    /// out, never captured earlier and carried in. Every message this turn
    /// produced, the assistant reply, the assistant-with-tool-calls
    /// message, each tool result, and any tool-returned image, is already
    /// pushed by the time a caller reaches this. The GUI persists whatever
    /// the last snapshot carried, so a snapshot taken any earlier writes a
    /// session file missing its own final messages.
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
        self.send_event(StreamEvent::TurnEnd {
            turn: turn + 1,
            finish_reason: finish_reason.to_string(),
            total_tokens: self.history.estimated_tokens(),
            prompt_cache_hit_tokens: stream_usage
                .as_ref()
                .map(|u| u.prompt_cache_hit_tokens)
                .unwrap_or(0),
            prompt_cache_miss_tokens: stream_usage
                .as_ref()
                .map(|u| u.prompt_cache_miss_tokens)
                .unwrap_or(0),
        });
    }

    fn arg_parse_error(&self, name: &str, args: &str, e: &serde_json::Error) -> String {
        if e.classify() != serde_json::error::Category::Eof {
            return format!("Tool error: Invalid input: {e}");
        }
        format!(
            "Tool error: the arguments for '{name}' stop part way through \
             ({e}). The reply ran into the {} token output cap while writing \
             them, so the call never finished. It carried {} characters. \
             Retry with a smaller payload: use `edit` to change part of a \
             file rather than `write` to replace all of it, or write the \
             file in several smaller calls. Raise `max_tokens` in \
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

    fn maybe_check_plain_language(&self, text: &str) -> bool {
        if !self.style_plain_language_flag.load(Ordering::SeqCst) {
            return false;
        }
        if text.len() < MIN_PLAIN_LANGUAGE_LENGTH {
            return false;
        }
        let grade = crate::style::flesch_kincaid_grade(text);
        let threshold = self.style_target_grade() + self.style_grade_tolerance;
        debug!(
            grade,
            target = self.style_target_grade(),
            tolerance = self.style_grade_tolerance,
            text_len = text.len(),
            "plain-language gate: grade {grade} vs threshold {threshold}"
        );
        grade > threshold
    }

    async fn revise_for_plain_language(&self, text: &str) -> StyleRevision {
        let max_attempts = self.style_max_revise_attempts;
        let mut current = text.to_string();
        let mut attempts = 0u32;

        let rubric = "Rewrite the following reply in plain language. \
            Use short sentences, common words, and active voice. \
            Cut padding, jargon, and passive constructions. \
            Preserve every technical fact, name, path, and code \
            reference exactly. Return only the rewritten reply, no \
            preamble or commentary.";

        let target_grade = self.style_target_grade();
        let tolerance = self.style_grade_tolerance;

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
                model: self.config.model.clone(),
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
                max_tokens: Some(self.config.max_tokens),
                thinking: None,
                thinking_mode: None,
                reasoning_effort: None,
                effort: Some(Effort::None),
            };

            match self.client.chat(&request).await {
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

    #[cfg(feature = "test-support")]
    pub fn maybe_check_plain_language_for_test(&self, text: &str) -> bool {
        self.maybe_check_plain_language(text)
    }

    #[cfg(feature = "test-support")]
    pub async fn revise_for_plain_language_for_test(&self, text: &str) -> (String, u32) {
        let rev = self.revise_for_plain_language(text).await;
        (rev.text, rev.attempts)
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
fn assistant_with_tools(text: &str, reasoning: &str, tool_calls: &[ToolCall]) -> Message {
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
fn merge_tool_call(accumulated: &mut Vec<ToolCall>, delta: &ToolCall) {
    let idx = delta.index;

    if let Some(existing) = accumulated.iter_mut().find(|tc| tc.index == idx) {
        if existing.id.is_empty() && !delta.id.is_empty() {
            existing.id = delta.id.clone();
        }
        if let Some(ref delta_func) = delta.function {
            let existing_func = existing.function.get_or_insert_with(Default::default);
            if let Some(ref name) = delta_func.name
                && existing_func.name.is_none()
            {
                debug!(index=?idx, name=%name, "merge_tool_call: set name");
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
    } else {
        let mut tc = delta.clone();
        let func = tc.function.get_or_insert_with(Default::default);
        if func.arguments.is_none() {
            func.arguments = Some(String::new());
        }
        debug!(
            index=?idx,
            name=?func.name,
            args_len = func.arguments.as_ref().map_or(0, |a| a.len()),
            "merge_tool_call: new"
        );
        accumulated.push(tc);
    }
}
