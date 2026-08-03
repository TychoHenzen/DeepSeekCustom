use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::api::client::DeepSeekClient;
use crate::api::types::{ChatRequest, Message, Role, ToolCall};
use crate::error::{HarnessError, Result};
use crate::tools::{ToolOutput, ToolRegistry};

use super::history::{MessageHistory, PruneReport};
use super::prompt::{SystemPromptBuilder, voice_mode_instructions};

/// Default token budget for the context pruning hysteresis oscillator.
/// History grows freely until it passes this high-water mark, then gets
/// pruned hard down to a third of it (see `context_low_water`).
const DEFAULT_CONTEXT_BUDGET: usize = 100_000;

/// Configuration for the agent loop.
pub struct AgentConfig {
    pub max_turns: u32,
    pub model: String,
    pub thinking: bool,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_turns: 100,
            model: "deepseek-v4-flash".into(),
            thinking: false,
        }
    }
}

/// Events sent from the agent loop to the TUI (or caller) during streaming.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// Text chunk received from the model.
    Text { turn: u32, text: String },
    /// A tool call has started.
    ToolCallStart {
        turn: u32,
        tool: String,
        args: String,
    },
    /// A tool call completed.
    ToolCallEnd {
        turn: u32,
        tool: String,
        output: String,
        is_error: bool,
    },
    /// The agent has finished its turn.
    TurnEnd {
        turn: u32,
        finish_reason: String,
        total_tokens: usize,
        prompt_cache_hit_tokens: u32,
        prompt_cache_miss_tokens: u32,
    },
    /// Session was reset.
    SessionReset,
    /// An error occurred.
    Error { message: String },
    /// Agent was interrupted by user (Escape key).
    Interrupted { message: String },
    /// Reasoning/thinking chunk received (when thinking is enabled).
    Reasoning { turn: u32, text: String },
    /// A repeat run started a new iteration.
    RepeatIterationStart { index: u32, total: u32 },
    /// A repeat run finished, whether by completing every iteration or by
    /// being interrupted partway through.
    RepeatFinished { completed: u32, total: u32 },
}

/// Core agent loop: user input → API call → tool execution → repeat.
pub struct AgentLoop {
    client: DeepSeekClient,
    tools: ToolRegistry,
    history: MessageHistory,
    config: AgentConfig,
    tx_events: Option<mpsc::UnboundedSender<StreamEvent>>,
    /// Flag set by GUI when user presses Escape to interrupt streaming.
    interrupt_flag: Arc<AtomicBool>,
    /// Shared flag: GUI sets this to enable/disable thinking.
    thinking_flag: Arc<AtomicBool>,
    /// Shared model name: GUI sets this when user changes model in settings.
    model_name: Arc<Mutex<String>>,
    /// Shared flag: GUI sets this when text to speech is turned on or off.
    voice_mode_flag: Arc<AtomicBool>,
    /// Shared token budget: the high-water mark for context pruning. Read
    /// straight off the atomic at the point of use, never mirrored into
    /// `AgentConfig`.
    context_budget: Arc<AtomicUsize>,
    /// Flag set by the GUI to stop a `run_repeat` loop between iterations.
    /// `run` consumes `interrupt_flag` internally. It swaps the flag back
    /// to false once it breaks out of a stream. So `interrupt_flag` cannot
    /// survive past one iteration. This flag is separate and stays set.
    repeat_interrupt_flag: Arc<AtomicBool>,
}

impl AgentLoop {
    /// Create a new AgentLoop.
    pub fn new(
        client: DeepSeekClient,
        tools: ToolRegistry,
        system_prompt: String,
        config: AgentConfig,
    ) -> Self {
        let thinking = config.thinking;
        let model = config.model.clone();
        Self {
            client,
            tools,
            history: MessageHistory::new(system_prompt),
            config,
            tx_events: None,
            interrupt_flag: Arc::new(AtomicBool::new(false)),
            thinking_flag: Arc::new(AtomicBool::new(thinking)),
            model_name: Arc::new(Mutex::new(model)),
            voice_mode_flag: Arc::new(AtomicBool::new(false)),
            context_budget: Arc::new(AtomicUsize::new(DEFAULT_CONTEXT_BUDGET)),
            repeat_interrupt_flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Set the event sender for streaming output.
    pub fn set_event_sender(&mut self, tx: mpsc::UnboundedSender<StreamEvent>) {
        self.tx_events = Some(tx);
    }

    /// Return a clone of the interrupt flag so the GUI can signal interruption.
    pub fn interrupt_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.interrupt_flag)
    }

    /// Return a clone of the thinking flag so the GUI can toggle thinking.
    pub fn thinking_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.thinking_flag)
    }

    /// Return a clone of the model name so the GUI can change the model.
    pub fn model_flag(&self) -> Arc<Mutex<String>> {
        Arc::clone(&self.model_name)
    }

    /// Return a clone of the voice mode flag so the GUI can toggle text to speech.
    pub fn voice_mode_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.voice_mode_flag)
    }

    /// Return a clone of the context budget flag so the GUI can change it.
    pub fn context_budget_flag(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.context_budget)
    }

    /// Return a clone of the repeat-interrupt flag. The GUI sets this to
    /// stop a `run_repeat` loop between iterations.
    pub fn repeat_interrupt_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.repeat_interrupt_flag)
    }

    /// Rebuild history from just the base system prompt, dropping every
    /// message and any voice-mode suffix. Used to start a repeat-runner
    /// iteration with a clean context. `sync_dynamic_config` restores the
    /// suffix on the next `run` call, so voice mode is unaffected.
    pub fn clear_history(&mut self) {
        self.history = MessageHistory::new(self.history.system_prompt().to_string());
    }

    /// Sync dynamic config from the GUI before building requests: thinking
    /// mode, model name, and the voice-mode system prompt suffix.
    fn sync_dynamic_config(&mut self) {
        self.config.thinking = self.thinking_flag.load(Ordering::SeqCst);
        if let Ok(model) = self.model_name.lock() {
            self.config.model.clone_from(&*model);
        }
        if self.voice_mode_flag.load(Ordering::SeqCst) {
            self.history
                .set_system_suffix(Some(voice_mode_instructions().to_string()));
        } else {
            self.history.set_system_suffix(None);
        }
    }

    /// Apply a hard prune down to a third of the budget, logging the report
    /// at `info` level. Called by `maybe_prune_context` once the history
    /// has passed the high-water mark. Pure aside from the log line, so
    /// it stays testable without a network.
    fn apply_prune(&mut self, scores: Option<&[f32]>) -> PruneReport {
        let budget = self.context_budget.load(Ordering::SeqCst);
        let report = self
            .history
            .prune_to_budget(context_low_water(budget), scores);
        info!(
            tokens_before = report.tokens_before,
            tokens_after = report.tokens_after,
            tool_bodies_elided = report.tool_bodies_elided,
            groups_collapsed = report.groups_collapsed,
            groups_dropped = report.groups_dropped,
            "context pruned"
        );
        report
    }

    /// Trip check for the hysteresis oscillator: a no-op API call under
    /// budget, a scored hard prune over it. A scoring failure degrades to
    /// oldest-first pruning rather than aborting or delaying the turn.
    async fn maybe_prune_context(&mut self) {
        let budget = self.context_budget.load(Ordering::SeqCst);
        if self.history.estimated_tokens() <= budget {
            return;
        }

        let messages: Vec<Message> = self.history.iter().cloned().collect();
        let scores = crate::context::relevance::score_messages(&self.client, &messages).await;
        if scores.is_none() {
            warn!("context pruning: relevance scoring failed, falling back to oldest-first order");
        }
        self.apply_prune(scores.as_deref());
    }

    /// Run the agent loop for a single user message.
    /// Returns the final assistant text or an error.
    pub async fn run(&mut self, user_input: &str) -> Result<Vec<String>> {
        self.sync_dynamic_config();

        // Add user message
        self.history.push(Message {
            role: Role::User,
            content: Some(user_input.to_string()),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        });

        let mut assistant_texts: Vec<String> = Vec::new();

        for turn in 0..self.config.max_turns {
            debug!("turn {}/{}", turn + 1, self.config.max_turns);

            self.maybe_prune_context().await;

            // Build API request
            let tools = self.tools.to_api_definitions();
            let messages = self.history.to_api_messages();

            let thinking_mode = if self.config.thinking {
                "thinking"
            } else {
                "non-thinking"
            };
            info!(thinking_mode = thinking_mode, "building API request");

            let request = ChatRequest {
                model: self.config.model.clone(),
                messages,
                tools: if tools.is_empty() { None } else { Some(tools) },
                tool_choice: None,
                stream: true,
                temperature: Some(0.7),
                max_tokens: Some(4096),
                thinking: None,
                thinking_mode: Some(thinking_mode.to_string()),
            };

            // Call API (streaming)
            let mut rx = self.client.chat_stream(&request);

            let mut stream_text = String::new();
            let mut stream_reasoning = String::new();
            let mut stream_tool_calls: Vec<ToolCall> = Vec::new();
            let mut finish_reason = String::new();
            let mut stream_usage: Option<crate::api::types::Usage> = None;

            while let Some(chunk_result) = rx.recv().await {
                // Check for user interrupt before processing chunk
                if self.interrupt_flag.load(Ordering::SeqCst) {
                    debug!("interrupt detected during stream receive");
                    break;
                }
                match chunk_result {
                    Ok(chunk) => {
                        if let Some(ref choices) = chunk.choices {
                            for choice in choices {
                                // Accumulate text
                                if let Some(ref content) = choice.delta.content {
                                    stream_text.push_str(content);
                                    self.send_event(StreamEvent::Text {
                                        turn: turn + 1,
                                        text: content.clone(),
                                    });
                                }
                                // Accumulate reasoning_content (must be echoed back to API)
                                if let Some(ref reasoning) = choice.delta.reasoning_content {
                                    stream_reasoning.push_str(reasoning);
                                    self.send_event(StreamEvent::Reasoning {
                                        turn: turn + 1,
                                        text: reasoning.clone(),
                                    });
                                }
                                // Merge tool call deltas by index
                                if let Some(ref tcs) = choice.delta.tool_calls {
                                    for tc in tcs {
                                        merge_tool_call(&mut stream_tool_calls, tc);
                                    }
                                }
                                // Track finish reason
                                if let Some(ref fr) = choice.finish_reason {
                                    finish_reason = fr.clone();
                                }
                            }
                        }
                        // Capture usage from final chunk (DeepSeek sends it with the last delta)
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
                "stream complete: text_len={}, reasoning_len={}, tool_calls={}, finish={}",
                stream_text.len(),
                stream_reasoning.len(),
                stream_tool_calls.len(),
                finish_reason,
            );

            self.send_event(StreamEvent::TurnEnd {
                turn: turn + 1,
                finish_reason: finish_reason.clone(),
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

            // Filter out tool calls lacking a function name (can appear as
            // empty deltas in V4 thinking mode during reasoning phase).
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
                    "filtered out {filtered_out} nameless tool call(s) from thinking delta"
                );
            }
            if !valid_tool_calls.is_empty() {
                // Use only valid tool calls for execution
                stream_tool_calls = valid_tool_calls;
                // Append assistant message with tool calls
                self.history.push(Message {
                    role: Role::Assistant,
                    content: if stream_text.is_empty() {
                        None
                    } else {
                        Some(stream_text.clone())
                    },
                    tool_calls: Some(stream_tool_calls.clone()),
                    tool_call_id: None,
                    reasoning_content: if stream_reasoning.is_empty() {
                        None
                    } else {
                        Some(stream_reasoning.clone())
                    },
                });

                // Execute each tool call
                for tc in &stream_tool_calls {
                    // Check for user interrupt before each tool call
                    if self.interrupt_flag.load(Ordering::SeqCst) {
                        info!("agent: user interrupted before tool execution");
                        self.interrupt_flag.store(false, Ordering::SeqCst);
                        self.send_event(StreamEvent::Interrupted {
                            message: "Interrupted by user (Escape)".into(),
                        });
                        return Ok(assistant_texts);
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

                    // Feed tool result back to history
                    self.history.push(Message {
                        role: Role::Tool,
                        content: Some(result.content),
                        tool_calls: None,
                        tool_call_id: Some(tc.id.clone()),
                        reasoning_content: None,
                    });
                }
            } else {
                // Text-only response — done
                info!(
                    turn = turn + 1,
                    text_len = stream_text.len(),
                    reasoning_len = stream_reasoning.len(),
                    "text-only response, completing turn"
                );
                if !stream_text.is_empty() {
                    assistant_texts.push(stream_text.clone());
                }
                self.history.push(Message {
                    role: Role::Assistant,
                    content: Some(stream_text.clone()),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: if stream_reasoning.is_empty() {
                        None
                    } else {
                        Some(stream_reasoning.clone())
                    },
                });
                break;
            }
        }

        // Max turns check
        if assistant_texts.is_empty() {
            warn!("max turns reached without final text response");
        }

        Ok(assistant_texts)
    }

    /// Execute a tool by name, handling SessionReset specially.
    async fn execute_tool(&self, name: &str, args: &str) -> ToolOutput {
        let input: serde_json::Value = match serde_json::from_str(args) {
            Ok(v) => v,
            Err(e) => {
                warn!("execute_tool: failed to parse args for '{}': {}", name, e);
                return ToolOutput {
                    content: format!("Tool error: Invalid input: {e}"),
                    is_error: true,
                };
            }
        };

        match self.tools.get(name) {
            Some(tool) => match tool.execute(input).await {
                Ok(output) => output,
                Err(HarnessError::SessionReset) => {
                    info!("session reset triggered by tool '{}'", name);
                    self.send_event(StreamEvent::SessionReset);
                    ToolOutput {
                        content: "Session reset initiated.".into(),
                        is_error: false,
                    }
                }
                Err(e) => ToolOutput {
                    content: format!("Tool error: {e}"),
                    is_error: true,
                },
            },
            None => ToolOutput {
                content: format!("Unknown tool: {name}"),
                is_error: true,
            },
        }
    }

    /// Send a StreamEvent to the TUI if a sender is configured.
    pub(crate) fn send_event(&self, event: StreamEvent) {
        if let Some(ref tx) = self.tx_events {
            let _ = tx.send(event);
        }
    }

    /// Send `StreamEvent::RepeatFinished`. Called by `repeat::run_repeat`
    /// once its loop ends, whichever way it ends.
    pub(crate) fn send_repeat_finished(&self, completed: u32, total: u32) {
        self.send_event(StreamEvent::RepeatFinished { completed, total });
    }

    /// Access the message history (for display).
    pub fn history(&self) -> &MessageHistory {
        &self.history
    }

    /// Clear history and reload system prompt (called after session reset).
    pub fn reset(&mut self, new_system_prompt: String, new_user_prompt: String) {
        info!("agent: session reset");
        self.history = MessageHistory::new(new_system_prompt);
        self.history.push(Message {
            role: Role::User,
            content: Some(new_user_prompt),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        });
        self.send_event(StreamEvent::SessionReset);
    }

    /// Rebuild system prompt from components (used after memory reload).
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
/// A prune drops history down to this level, not back up to the budget.
/// That buys headroom for several turns before it fires again.
fn context_low_water(budget: usize) -> usize {
    budget / 3
}

/// Merge a streaming tool call delta into the accumulated tool calls list.
///
/// DeepSeek streams tool calls across multiple chunks:
/// - First chunk: `{index: 0, id: "call_xxx", function: {name: "read", arguments: ""}}`
/// - Subsequent chunks: `{index: 0, function: {arguments: "more_json"}}`
///
/// Matches by index and merges partial fields.
fn merge_tool_call(accumulated: &mut Vec<ToolCall>, delta: &ToolCall) {
    let idx = delta.index;

    // Find existing entry by index
    if let Some(existing) = accumulated.iter_mut().find(|tc| tc.index == idx) {
        // Merge id (first chunk has it)
        if existing.id.is_empty() && !delta.id.is_empty() {
            existing.id = delta.id.clone();
        }
        // Merge function fields
        if let Some(ref delta_func) = delta.function {
            let existing_func = existing.function.get_or_insert_with(Default::default);
            if let Some(ref name) = delta_func.name {
                if existing_func.name.is_none() {
                    debug!(index=?idx, name=%name, "merge_tool_call: set name");
                    existing_func.name = Some(name.clone());
                }
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
        // New tool call — ensure function and arguments exist
        let mut tc = delta.clone();
        let func = tc.function.get_or_insert_with(Default::default);
        if func.arguments.is_none() {
            func.arguments = Some(String::new());
        }
        debug!(
            index=?idx,
            name=?func.name,
            args_len=func.arguments.as_ref().map_or(0, |a| a.len()),
            "merge_tool_call: new"
        );
        accumulated.push(tc);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::tools::{Tool, ToolOutput, ToolRegistry};

    struct EchoTool;
    #[async_trait::async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "echoes input"
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn execute(&self, _input: serde_json::Value) -> Result<ToolOutput> {
            Ok(ToolOutput {
                content: "echoed".into(),
                is_error: false,
            })
        }
    }

    #[test]
    fn agent_config_defaults() {
        let cfg = AgentConfig::default();
        assert_eq!(cfg.max_turns, 100);
        assert_eq!(cfg.model, "deepseek-v4-flash");
        assert!(!cfg.thinking);
    }

    #[test]
    fn agent_loop_creates_with_history() {
        let client = DeepSeekClient::new("sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let agent = AgentLoop::new(client, tools, "test prompt".into(), AgentConfig::default());
        assert_eq!(agent.history().len(), 0);
    }

    #[test]
    fn session_reset_clears_history() {
        let client = DeepSeekClient::new("sk-test".into(), None, None);
        let mut tools = ToolRegistry::new();
        tools.register(Arc::new(EchoTool));
        let mut agent = AgentLoop::new(client, tools, "initial".into(), AgentConfig::default());

        // Push a user message then reset
        agent.history.push(Message::user("hello".into()));
        assert_eq!(agent.history().len(), 1);

        agent.reset("fresh prompt".into(), "restart".into());
        // After reset: fresh system + 1 user message
        assert_eq!(agent.history().len(), 1);
    }

    #[test]
    fn voice_mode_flag_defaults_to_false() {
        let client = DeepSeekClient::new("sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let agent = AgentLoop::new(client, tools, "test".into(), AgentConfig::default());
        assert!(
            !agent
                .voice_mode_flag()
                .load(std::sync::atomic::Ordering::SeqCst)
        );
    }

    #[test]
    fn voice_mode_flag_handle_observes_writes() {
        let client = DeepSeekClient::new("sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let agent = AgentLoop::new(client, tools, "test".into(), AgentConfig::default());
        let flag = agent.voice_mode_flag();
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(
            agent
                .voice_mode_flag()
                .load(std::sync::atomic::Ordering::SeqCst)
        );
    }

    #[test]
    fn sync_dynamic_config_sets_voice_suffix_when_flag_true() {
        let client = DeepSeekClient::new("sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let mut agent = AgentLoop::new(client, tools, "sys prompt".into(), AgentConfig::default());
        agent
            .voice_mode_flag()
            .store(true, std::sync::atomic::Ordering::SeqCst);

        agent.sync_dynamic_config();

        let api = agent.history().to_api_messages();
        assert!(
            api[0]
                .content
                .as_deref()
                .unwrap()
                .contains("## Voice reply mode")
        );
    }

    #[test]
    fn sync_dynamic_config_clears_voice_suffix_when_flag_false() {
        let client = DeepSeekClient::new("sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let mut agent = AgentLoop::new(client, tools, "sys prompt".into(), AgentConfig::default());
        agent
            .voice_mode_flag()
            .store(true, std::sync::atomic::Ordering::SeqCst);
        agent.sync_dynamic_config();

        agent
            .voice_mode_flag()
            .store(false, std::sync::atomic::Ordering::SeqCst);
        agent.sync_dynamic_config();

        let api = agent.history().to_api_messages();
        assert!(
            !api[0]
                .content
                .as_deref()
                .unwrap()
                .contains("## Voice reply mode")
        );
    }

    #[test]
    fn context_low_water_is_a_third_of_budget() {
        assert_eq!(context_low_water(100_000), 33_333);
        assert_eq!(context_low_water(300), 100);
    }

    #[test]
    fn context_budget_flag_defaults_to_100000() {
        let client = DeepSeekClient::new("sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let agent = AgentLoop::new(client, tools, "test".into(), AgentConfig::default());
        assert_eq!(
            agent
                .context_budget_flag()
                .load(std::sync::atomic::Ordering::SeqCst),
            DEFAULT_CONTEXT_BUDGET
        );
    }

    #[test]
    fn context_budget_flag_handle_observes_writes() {
        let client = DeepSeekClient::new("sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let agent = AgentLoop::new(client, tools, "test".into(), AgentConfig::default());
        let flag = agent.context_budget_flag();
        flag.store(42_000, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            agent
                .context_budget_flag()
                .load(std::sync::atomic::Ordering::SeqCst),
            42_000
        );
    }

    #[test]
    fn apply_prune_is_noop_under_budget() {
        let client = DeepSeekClient::new("sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let mut agent = AgentLoop::new(client, tools, "sys".into(), AgentConfig::default());
        agent.history.push(Message::user("hello".into()));
        let before = agent.history().estimated_tokens();

        let report = agent.apply_prune(None);

        assert_eq!(report.tokens_before, before);
        assert_eq!(report.tokens_after, before);
        assert_eq!(agent.history().estimated_tokens(), before);
        assert_eq!(agent.history().len(), 1);
    }

    #[test]
    fn apply_prune_reduces_oversized_history_to_low_water() {
        let client = DeepSeekClient::new("sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let mut agent = AgentLoop::new(client, tools, "sys".into(), AgentConfig::default());

        // Push far more content than a small test budget allows. The last
        // two groups are pinned and never touched by any tier, so keep
        // them short. A large pinned tail would set a floor above the
        // low water mark, and no amount of pruning could reach it.
        for i in 0..18 {
            agent
                .history
                .push(Message::user(format!("question {i} {}", "x".repeat(200))));
            agent
                .history
                .push(Message::assistant(format!("answer {i} {}", "x".repeat(200))));
        }
        agent.history.push(Message::user("hi".into()));
        agent.history.push(Message::assistant("ok".into()));
        agent.history.push(Message::user("bye".into()));
        agent.history.push(Message::assistant("ok".into()));

        agent
            .context_budget_flag()
            .store(200, std::sync::atomic::Ordering::SeqCst);

        let report = agent.apply_prune(None);

        assert!(agent.history().estimated_tokens() <= context_low_water(200));
        assert_eq!(report.tokens_after, agent.history().estimated_tokens());
        assert!(report.tokens_after < report.tokens_before);
        assert!(
            report.tool_bodies_elided > 0
                || report.groups_collapsed > 0
                || report.groups_dropped > 0
        );
    }

    #[test]
    fn apply_prune_with_none_scores_does_not_panic() {
        let client = DeepSeekClient::new("sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let mut agent = AgentLoop::new(client, tools, "sys".into(), AgentConfig::default());
        for i in 0..10 {
            agent.history.push(Message::user(format!("q{i}")));
            agent.history.push(Message::assistant(format!("a{i}")));
        }
        agent
            .context_budget_flag()
            .store(1, std::sync::atomic::Ordering::SeqCst);

        let report = agent.apply_prune(None);
        assert!(report.tokens_after <= report.tokens_before);
    }

    #[test]
    fn rebuild_system_prompt_clears_messages() {
        let client = DeepSeekClient::new("sk-test".into(), None, None);
        let mut tools = ToolRegistry::new();
        tools.register(Arc::new(EchoTool));
        let mut agent = AgentLoop::new(client, tools, "initial".into(), AgentConfig::default());
        agent.history.push(Message::user("hi".into()));

        agent.rebuild_system_prompt(Some("memory"), Some("skills"));
        assert_eq!(agent.history().len(), 0);
    }

    #[tokio::test]
    async fn execute_tool_returns_output() {
        let client = DeepSeekClient::new("sk-test".into(), None, None);
        let mut tools = ToolRegistry::new();
        tools.register(Arc::new(EchoTool));
        let agent = AgentLoop::new(client, tools, "test".into(), AgentConfig::default());

        let output = agent.execute_tool("echo", "{}").await;
        assert!(!output.is_error);
        assert_eq!(output.content, "echoed");
    }

    #[tokio::test]
    async fn execute_unknown_tool_returns_error() {
        let client = DeepSeekClient::new("sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let agent = AgentLoop::new(client, tools, "test".into(), AgentConfig::default());

        let output = agent.execute_tool("nonexistent", "{}").await;
        assert!(output.is_error);
        assert!(output.content.contains("Unknown tool"));
    }

    /// Integration test: spawn a mock HTTP server returning SSE with reasoning_content,
    /// run the full agent loop, and verify Reasoning events are emitted.
    #[tokio::test]
    async fn thinking_enabled_emits_reasoning_events() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        // Bind to port 0 so the OS assigns a free port
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();

        // SSE response with reasoning_content, then text, then stop + [DONE]
        let response_body = concat!(
            "data: {\"id\":\"t1\",\"object\":\"chat.completion.chunk\",\"created\":1,",
            "\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":",
            "{\"content\":null,\"reasoning_content\":\"I should think about this\"},",
            "\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"t1\",\"object\":\"chat.completion.chunk\",\"created\":1,",
            "\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":",
            "{\"content\":\"The answer is 42\"},",
            "\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"t1\",\"object\":\"chat.completion.chunk\",\"created\":1,",
            "\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{},",
            "\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        );

        // Spawn mock server thread
        let response = response_body.to_string();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            // Read the HTTP request (header part)
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            // Write HTTP response
            let http_response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}",
                response.len(),
                response,
            );
            let _ = stream.write_all(http_response.as_bytes());
            let _ = stream.flush();
            // Keep connection alive briefly so client can read
            thread::sleep(std::time::Duration::from_millis(200));
        });

        // Create client pointing at mock server
        let client = DeepSeekClient::new(
            "sk-test".into(),
            Some(format!("http://127.0.0.1:{port}")),
            Some("deepseek-v4-flash".into()),
        );

        let tools = ToolRegistry::new();
        let mut config = AgentConfig::default();
        config.thinking = true;

        let mut agent = AgentLoop::new(client, tools, "sys".into(), config);

        // Capture events via channel
        let (tx, mut rx) = mpsc::unbounded_channel();
        agent.set_event_sender(tx);

        // Run the agent
        let result = agent.run("hello").await;
        assert!(
            result.is_ok(),
            "agent run should succeed: {:?}",
            result.err()
        );
        let responses = result.unwrap();
        assert!(!responses.is_empty(), "should have response text");

        // Collect all events
        let mut events: Vec<StreamEvent> = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }

        // Verify Reasoning events were emitted
        let reasoning_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, StreamEvent::Reasoning { .. }))
            .collect();
        assert!(
            !reasoning_events.is_empty(),
            "expected at least one Reasoning event, got events: {:?}",
            events
                .iter()
                .map(|e| format!("{:?}", e))
                .collect::<Vec<_>>()
                .join(", ")
        );

        // Verify reasoning content is correct
        let reasoning: String = reasoning_events
            .iter()
            .filter_map(|e| {
                if let StreamEvent::Reasoning { text, .. } = e {
                    Some(text.clone())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(reasoning, "I should think about this");

        // Verify Text events were also emitted
        let text_count = events
            .iter()
            .filter(|e| matches!(e, StreamEvent::Text { .. }))
            .count();
        assert!(text_count > 0, "expected at least one Text event");
    }

    #[test]
    fn clear_history_drops_messages_keeps_system_prompt() {
        let client = DeepSeekClient::new("sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let mut agent = AgentLoop::new(client, tools, "sys prompt".into(), AgentConfig::default());
        agent.history.push(Message::user("hello".into()));
        assert_eq!(agent.history().len(), 1);

        agent.clear_history();

        assert_eq!(agent.history().len(), 0);
        let api = agent.history().to_api_messages();
        assert_eq!(api.len(), 1);
        assert_eq!(api[0].content.as_deref(), Some("sys prompt"));
    }

    #[test]
    fn clear_history_after_voice_suffix_still_produces_working_system_message() {
        let client = DeepSeekClient::new("sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let mut agent = AgentLoop::new(client, tools, "sys prompt".into(), AgentConfig::default());
        agent
            .voice_mode_flag()
            .store(true, std::sync::atomic::Ordering::SeqCst);
        agent.sync_dynamic_config();
        agent.history.push(Message::user("hello".into()));

        agent.clear_history();

        assert_eq!(agent.history().len(), 0);
        let api = agent.history().to_api_messages();
        assert_eq!(api.len(), 1);
        assert_eq!(api[0].content.as_deref(), Some("sys prompt"));

        // sync_dynamic_config still works after clear_history and restores
        // the voice suffix on the next turn.
        agent.sync_dynamic_config();
        let api = agent.history().to_api_messages();
        assert!(
            api[0]
                .content
                .as_deref()
                .unwrap()
                .contains("## Voice reply mode")
        );
    }

    #[tokio::test]
    async fn run_repeat_zero_iterations_emits_only_repeat_finished() {
        let client = DeepSeekClient::new("sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let mut agent = AgentLoop::new(client, tools, "sys".into(), AgentConfig::default());
        let (tx, mut rx) = mpsc::unbounded_channel();
        agent.set_event_sender(tx);

        super::super::repeat::run_repeat(&mut agent, "do the thing", 0).await;

        let mut events: Vec<StreamEvent> = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::RepeatFinished { completed, total } => {
                assert_eq!(*completed, 0);
                assert_eq!(*total, 0);
            }
            other => panic!("expected RepeatFinished, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_repeat_stops_immediately_when_interrupt_flag_already_set() {
        let client = DeepSeekClient::new("sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let mut agent = AgentLoop::new(client, tools, "sys".into(), AgentConfig::default());
        let (tx, mut rx) = mpsc::unbounded_channel();
        agent.set_event_sender(tx);
        agent
            .repeat_interrupt_flag()
            .store(true, std::sync::atomic::Ordering::SeqCst);

        super::super::repeat::run_repeat(&mut agent, "do the thing", 3).await;

        let mut events: Vec<StreamEvent> = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }
        // No iteration should have started.
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, StreamEvent::RepeatIterationStart { .. }))
        );
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::RepeatFinished { completed, total } => {
                assert_eq!(*completed, 0);
                assert_eq!(*total, 3);
            }
            other => panic!("expected RepeatFinished, got {other:?}"),
        }
    }

    /// Integration test: spawn a mock HTTP server that serves two
    /// connections, each a minimal text-only SSE response, and run
    /// `run_repeat` for two iterations against it.
    #[tokio::test]
    async fn run_repeat_two_iterations_against_mock_server() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();

        fn response_for(text: &str) -> String {
            format!(
                concat!(
                    "data: {{\"id\":\"t1\",\"object\":\"chat.completion.chunk\",\"created\":1,",
                    "\"model\":\"m\",\"choices\":[{{\"index\":0,\"delta\":",
                    "{{\"content\":\"{}\"}},",
                    "\"finish_reason\":null}}]}}\n\n",
                    "data: {{\"id\":\"t1\",\"object\":\"chat.completion.chunk\",\"created\":1,",
                    "\"model\":\"m\",\"choices\":[{{\"index\":0,\"delta\":{{}},",
                    "\"finish_reason\":\"stop\"}}]}}\n\n",
                    "data: [DONE]\n\n",
                ),
                text
            )
        }

        thread::spawn(move || {
            for i in 0..2 {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let body = response_for(&format!("answer {i}"));
                let http_response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body,
                );
                let _ = stream.write_all(http_response.as_bytes());
                let _ = stream.flush();
                thread::sleep(std::time::Duration::from_millis(100));
            }
        });

        let client = DeepSeekClient::new(
            "sk-test".into(),
            Some(format!("http://127.0.0.1:{port}")),
            Some("deepseek-v4-flash".into()),
        );

        let tools = ToolRegistry::new();
        let mut agent = AgentLoop::new(client, tools, "sys".into(), AgentConfig::default());
        let (tx, mut rx) = mpsc::unbounded_channel();
        agent.set_event_sender(tx);

        super::super::repeat::run_repeat(&mut agent, "do the task", 2).await;

        let mut events: Vec<StreamEvent> = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }

        let starts: Vec<(u32, u32)> = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::RepeatIterationStart { index, total } => Some((*index, *total)),
                _ => None,
            })
            .collect();
        assert_eq!(starts, vec![(1, 2), (2, 2)]);

        let finished = events
            .iter()
            .find(|e| matches!(e, StreamEvent::RepeatFinished { .. }))
            .expect("expected a RepeatFinished event");
        match finished {
            StreamEvent::RepeatFinished { completed, total } => {
                assert_eq!(*completed, 2);
                assert_eq!(*total, 2);
            }
            _ => unreachable!(),
        }

        // History at the end should hold only the last iteration's
        // messages: the user message plus the assistant reply.
        assert_eq!(agent.history().len(), 2);
        let api = agent.history().to_api_messages();
        let last_assistant = api
            .iter()
            .rev()
            .find(|m| m.role == crate::api::types::Role::Assistant)
            .expect("expected an assistant message");
        assert_eq!(last_assistant.content.as_deref(), Some("answer 1"));
    }
}
