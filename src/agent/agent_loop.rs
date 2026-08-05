use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::api::client::{ApiClient, Provider};
use crate::api::types::{ChatRequest, Content, ContentPart, ImageAttachment, Message, Role, ToolCall};
use crate::effort::Effort;
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
    pub effort: Effort,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_turns: 100,
            model: "deepseek-v4-flash".into(),
            effort: Effort::None,
        }
    }
}

/// Commands sent from the GUI to the agent task over the input channel.
/// This is the asymmetric partner of `StreamEvent`, which carries events
/// the other way. A bare `String` could only ever mean "run this turn",
/// which left session switching with no way to reach the backend.
#[derive(Debug)]
pub enum AgentCommand {
    /// Run one user turn with this text and, when the user pasted or
    /// dropped one, the image to send alongside it. Text and image travel
    /// together on one command so a single `send` is one indivisible turn:
    /// two turns sent back to back can never cross-pair their images, and
    /// nothing needs a side channel to carry per-turn payload.
    UserTurn {
        text: String,
        image: Option<ImageAttachment>,
    },
    /// Start a fresh, empty conversation.
    NewSession,
    /// Load a saved conversation. `messages` restores the `Api` backend's
    /// history. `claude_session_id` is stored for a later `--resume` on
    /// the `ClaudeCli` backend. This carries only what the agent needs,
    /// not a whole `SessionRecord`: the display transcript stays in the
    /// GUI, the only thing that renders it.
    LoadSession {
        messages: Vec<Message>,
        claude_session_id: Option<String>,
    },
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
    /// A snapshot of the conversation, sent alongside `TurnEnd` so the GUI
    /// can persist a session without owning `MessageHistory` itself.
    /// `messages` is the API history on the `Api` path. On the `ClaudeCli`
    /// path there is no `MessageHistory` at all, so `messages` is always
    /// empty there: the display transcript plus `claude_session_id` is the
    /// whole conversation on that path, and an empty vector is correct,
    /// not a gap.
    ConversationSnapshot {
        messages: Vec<Message>,
        claude_session_id: Option<String>,
    },
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

/// Identifies one subagent dispatch, for event routing. Cheap to copy and
/// compare, and usable as a map key: a later phase keys a subagent
/// registry by it. Allocated by `run_subagent` in
/// `src/backend/subagent.rs`, one id per dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SubagentId(u64);

impl SubagentId {
    /// Allocate a fresh, process-unique id. Each call returns a distinct
    /// value, so two concurrent dispatches never collide.
    pub fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        SubagentId(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

impl std::fmt::Display for SubagentId {
    /// Renders as the bare numeric id, the same form `Task`'s tool output
    /// puts in front of the model when a `keep_open` dispatch stays open,
    /// so a later `SendMessage` call can quote it back verbatim.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for SubagentId {
    type Err = std::num::ParseIntError;

    /// Parses the decimal form `Display` renders. This is how the
    /// `SendMessage` tool turns its `session_id` input back into a
    /// `SubagentId`, matching the id `Task`'s `keep_open` result names.
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(SubagentId(s.parse()?))
    }
}

/// The dispatch-time facts about a subagent that the transcript needs to
/// draw its block, but that no `StreamEvent` carries on its own: which
/// backend it runs on, which model resolved for it, and how deep in the
/// dispatch chain it sits. Captured once, at dispatch time, in
/// `run_subagent` (`src/backend/subagent.rs`), and carried on the first
/// `RouteHop` a forwarder ever prepends for that subagent, so the
/// transcript can fill in a `Subagent` block the moment it first sees the
/// id, with no separate "subagent started" event needed.
#[derive(Debug, Clone)]
pub struct SubagentMeta {
    pub backend: String,
    pub model: String,
    pub depth: u32,
}

/// One hop in a `RoutedEvent`'s route: the subagent that relayed the event
/// upward, plus the dispatch-time facts about it. Carrying `meta` on every
/// hop, not just the first, means the transcript never has to remember
/// whether it already recorded a given subagent's backend and model: it is
/// simply present on every event that subagent ever forwards.
///
/// `session_turns` and `send_message_calls` are not dispatch-time facts:
/// they change turn by turn, so every event carries the values current at
/// the moment it was forwarded rather than a value fixed once at dispatch.
/// `session_turns` is how many turns the session named by `id` has run so
/// far, against `session_turn_cap`. `send_message_calls` is the total
/// `SendMessage` calls the session's owner has made so far during its own
/// current turn, across every session it has open, against
/// `send_message_call_cap`. See "both counts visible in the subagent
/// block header" in the roadmap's Phase 3 section
/// (`docs/plans/2026-08-04-long-term-roadmap.md`).
#[derive(Debug, Clone)]
pub struct RouteHop {
    pub id: SubagentId,
    pub meta: SubagentMeta,
    pub session_turns: u32,
    pub session_turn_cap: u32,
    pub send_message_calls: u32,
    pub send_message_call_cap: u32,
}

/// One `StreamEvent`, tagged with the chain of subagents it passed through
/// on its way up to the top of the dispatch tree. Empty for an event the
/// main session emits directly. A depth-1 subagent's own event carries a
/// one-element route. A depth-2 subagent's event carries two elements,
/// outermost first: `route[0]` is the subagent the main session dispatched
/// directly, `route[1]` is the one it in turn dispatched.
///
/// `StreamEvent` itself gains no new variants for this. Every sender of
/// events, `AgentLoop` and `ClaudeCliDriver` alike, wraps its own event
/// with an empty route. A subagent's forwarder (`src/backend/subagent.rs`)
/// prepends its own id and meta to the route of every event it relays
/// upward, so nesting accumulates a route for free as an event travels up
/// the chain.
#[derive(Debug, Clone)]
pub struct RoutedEvent {
    pub route: Vec<RouteHop>,
    pub event: StreamEvent,
}

impl RoutedEvent {
    /// Wrap `event` with an empty route: the shape every direct sender
    /// (an `AgentLoop` or a `ClaudeCliDriver`, main session or subagent)
    /// uses for its own events. Routing is added later, only by a
    /// forwarder relaying the event up from a nested dispatch.
    pub fn own(event: StreamEvent) -> Self {
        Self {
            route: Vec::new(),
            event,
        }
    }
}

/// Core agent loop: user input → API call → tool execution → repeat.
pub struct AgentLoop {
    client: ApiClient,
    tools: ToolRegistry,
    history: MessageHistory,
    config: AgentConfig,
    tx_events: Option<mpsc::UnboundedSender<RoutedEvent>>,
    /// Flag set by GUI when user presses Escape to interrupt streaming.
    /// Injected at construction rather than created here, so a subagent
    /// built through `BackendFactory` can share the same flag the GUI
    /// holds for the main session. Escape then reaches a dispatched
    /// subagent, not just the turn in front of the user.
    interrupt_flag: Arc<AtomicBool>,
    /// Shared flag: GUI sets this to the current reasoning-effort level,
    /// encoded as a `u8` via `Effort::to_u8`/`Effort::from_u8`. Replaces the
    /// old `thinking_flag: Arc<AtomicBool>`, which could only ever mean
    /// "high or none".
    effort_flag: Arc<AtomicU8>,
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
    /// Live subagent sessions this agent may have opened through a future
    /// `keep_open` dispatch. `None` until `set_subagent_registry` is
    /// called; `BackendFactory` gives every `Api` backend it builds a
    /// fresh registry of its own, not one shared across the dispatch tree.
    /// That is what makes ownership real: a session belongs to the agent
    /// that opened it, and only that agent's registry can hold it. `run`
    /// and the `SessionReset` branch of `execute_tool` both close every
    /// entry on this agent's own registry, enforcing the lifetime rule: a
    /// session cannot outlive the turn of the agent that opened it, and
    /// `Reset` closes every session that agent owns.
    subagent_registry: Option<Arc<crate::backend::registry::SubagentRegistry>>,
    /// Shared working directory, the same one `BashTool`, `ReadTool`, and
    /// `WriteTool` read fresh on every call. `None` until
    /// `set_working_dir` is called; `BackendFactory` gives every `Api`
    /// backend it builds its own `working_dir()` handle. Read fresh every
    /// turn in `sync_dynamic_config`, the same pattern as the thinking and
    /// voice-mode flags, so the model is never told a directory it was
    /// handed once at startup and never revisited.
    working_dir: Option<Arc<Mutex<PathBuf>>>,
}

impl AgentLoop {
    /// Create a new AgentLoop. `interrupt_flag` comes from the caller
    /// rather than being created here, so `BackendFactory` can hand every
    /// backend it builds, main session or subagent, the same shared flag.
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
        }
    }

    /// Set the event sender for streaming output.
    pub fn set_event_sender(&mut self, tx: mpsc::UnboundedSender<RoutedEvent>) {
        self.tx_events = Some(tx);
    }

    /// Give this agent a handle to the subagent registry it should close
    /// out on every turn end and on every reset. `BackendFactory` calls
    /// this on every `Api` backend it builds, main session or subagent.
    pub fn set_subagent_registry(&mut self, registry: Arc<crate::backend::registry::SubagentRegistry>) {
        self.subagent_registry = Some(registry);
    }

    /// Give this agent a handle to the shared working directory the tools
    /// act against, the same `Arc` `BackendFactory::working_dir` hands to
    /// `BashTool`, `ReadTool`, and `WriteTool`. `sync_dynamic_config` reads
    /// it fresh every turn, so a change other than through this agent's
    /// own tools (a GUI control, a future `Cd` tool) still shows up in the
    /// next turn's system prompt.
    pub fn set_working_dir(&mut self, working_dir: Arc<Mutex<PathBuf>>) {
        self.working_dir = Some(working_dir);
    }

    /// Replace this agent's own effort flag with one the caller already
    /// holds, in place of the fresh one `new` created from `config.effort`.
    /// `BackendFactory` calls this right after construction so this agent's
    /// own `effort_flag()` and the `Arc` its own `Task` tool was given as
    /// `parent_effort_flag` are the exact same object. Whichever value ends
    /// up seeded into it, at startup from settings or later from a GUI
    /// control, is then visible immediately to a `Task` dispatch's own
    /// "inherit the session's current level" default, with no separate
    /// sync step needed.
    pub fn set_effort_flag(&mut self, effort_flag: Arc<AtomicU8>) {
        self.effort_flag = effort_flag;
    }

    /// The subagent registry this agent owns, if any. Test-only: used by
    /// `factory_tests.rs` to prove two agents `BackendFactory` builds get
    /// distinct registries rather than one shared across the dispatch tree.
    #[cfg(test)]
    pub(crate) fn subagent_registry_for_test(
        &self,
    ) -> Option<Arc<crate::backend::registry::SubagentRegistry>> {
        self.subagent_registry.clone()
    }

    /// Return a clone of the interrupt flag so the GUI can signal interruption.
    pub fn interrupt_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.interrupt_flag)
    }

    /// Return a clone of the effort flag so the GUI can change the
    /// reasoning-effort level.
    pub fn effort_flag(&self) -> Arc<AtomicU8> {
        Arc::clone(&self.effort_flag)
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

    /// Test-only: names of every registered tool. Lets a test in another
    /// module (`src/backend/factory_tests.rs`) assert on depth-gated tool
    /// registration without a production getter over the tool registry.
    #[cfg(test)]
    pub(crate) fn tool_names(&self) -> Vec<String> {
        self.tools.list().iter().map(|t| t.name().to_string()).collect()
    }

    /// Rebuild history from just the base system prompt, dropping every
    /// message and any voice-mode suffix. Used to start a repeat-runner
    /// iteration with a clean context. `sync_dynamic_config` restores the
    /// suffix on the next `run` call, so voice mode is unaffected.
    pub fn clear_history(&mut self) {
        self.history = MessageHistory::new(self.history.system_prompt().to_string());
    }

    /// Replace the message history with a saved conversation's messages,
    /// for loading a session from disk. See `MessageHistory::restore` for
    /// the exact guarantees: replaces rather than appends, leaves the
    /// system prompt and suffix untouched, recomputes the token count.
    pub fn restore_history(&mut self, messages: Vec<Message>) {
        self.history.restore(messages);
    }

    /// Sync dynamic config from the GUI before building requests: thinking
    /// mode, model name, the voice-mode system prompt suffix, and the
    /// working directory line.
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
        if let Some(working_dir) = &self.working_dir {
            if let Ok(dir) = working_dir.lock() {
                self.history.set_working_dir(Some(dir.display().to_string()));
            }
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
            images_elided = report.images_elided,
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
        let scores = crate::context::relevance::score_messages(
            &self.client,
            &messages,
            &self.config.model,
        )
        .await;
        if scores.is_none() {
            warn!("context pruning: relevance scoring failed, falling back to oldest-first order");
        }
        self.apply_prune(scores.as_deref());
    }

    /// Run the agent loop for a single user message.
    /// Returns the final assistant text or an error.
    ///
    /// Wraps `run_turn`: whichever way that returns, success, an error, or
    /// an interrupt, this closes every subagent session this turn opened
    /// before handing the result back. That is the lifetime rule from the
    /// roadmap's Phase 3: a session lives until its parent's turn ends, or
    /// until it is closed, whichever comes first. Wrapping the whole
    /// method, rather than adding a close call at each of `run_turn`'s own
    /// return points, means a future return point added there cannot
    /// forget it.
    pub async fn run(&mut self, user_input: &str) -> Result<Vec<String>> {
        self.run_with_image(user_input, None).await
    }

    /// Run the agent loop for a single user message, with an optional image
    /// attachment. See `build_user_content` for how the image maps per
    /// provider: dropped with a transcript notice for DeepSeek, sent as an
    /// `image_url` content part for Ollama. `run` is this method called
    /// with no image, so a turn with no attachment is unaffected.
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

    /// The turn body `run_with_image` wraps. See `run`'s own doc comment
    /// for why the subagent-closing step lives outside this method instead
    /// of inside it.
    async fn run_turn(
        &mut self,
        user_input: &str,
        image: Option<&ImageAttachment>,
    ) -> Result<Vec<String>> {
        self.sync_dynamic_config();

        let (content, notice) = build_user_content(self.client.provider(), user_input, image);
        if let Some(message) = notice {
            self.send_event(StreamEvent::Error { message });
        }

        // Add user message
        self.history.push(Message {
            role: Role::User,
            content: Some(content),
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

            let effort = self.config.effort;
            info!(effort = ?effort, "building API request");

            let request = ChatRequest {
                model: self.config.model.clone(),
                messages,
                tools: if tools.is_empty() { None } else { Some(tools) },
                tool_choice: None,
                stream: true,
                temperature: Some(0.7),
                max_tokens: Some(4096),
                thinking: None,
                thinking_mode: None,
                reasoning_effort: None,
                effort: Some(effort),
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

            self.send_event(StreamEvent::ConversationSnapshot {
                messages: self.history.iter().cloned().collect(),
                claude_session_id: None,
            });
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
                        Some(Content::text(stream_text.clone()))
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

                    let tool_image = result.image;

                    // Feed tool result back to history
                    self.history.push(Message {
                        role: Role::Tool,
                        content: Some(Content::text(result.content)),
                        tool_calls: None,
                        tool_call_id: Some(tc.id.clone()),
                        reasoning_content: None,
                    });

                    // A tool-returned image cannot travel inside the tool
                    // result message itself: `ToolOutput::image`'s doc
                    // comment explains why. Carry it instead as a synthetic
                    // user turn right after the tool result, mapped through
                    // the same per-provider logic a pasted or dropped image
                    // already goes through, so it reaches the model on the
                    // next request built this loop.
                    if let Some(image) = tool_image {
                        let (content, notice) = build_user_content(
                            self.client.provider(),
                            "(image read by the tool call above)",
                            Some(&image),
                        );
                        if let Some(message) = notice {
                            self.send_event(StreamEvent::Error { message });
                        }
                        self.history.push(Message {
                            role: Role::User,
                            content: Some(content),
                            tool_calls: None,
                            tool_call_id: None,
                            reasoning_content: None,
                        });
                    }
                }
            } else {
                // Text-only response - done
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
                    content: Some(Content::text(stream_text.clone())),
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
    ///
    /// `pub(crate)`, not private: `factory_tests.rs`
    /// (`backend::factory_tests`) drives a reset directly through this to
    /// prove the subagent-registry lifetime rule holds across two agents
    /// built from the same `BackendFactory`, without needing a live HTTP
    /// call the way exercising it through `run` would.
    pub(crate) async fn execute_tool(&self, name: &str, args: &str) -> ToolOutput {
        let input: serde_json::Value = match serde_json::from_str(args) {
            Ok(v) => v,
            Err(e) => {
                warn!("execute_tool: failed to parse args for '{}': {}", name, e);
                return ToolOutput {
                    content: format!("Tool error: Invalid input: {e}"),
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
                content: format!("Unknown tool: {name}"),
                is_error: true,
                image: None,
            },
        }
    }

    /// Send a StreamEvent to the TUI if a sender is configured. Wrapped
    /// with an empty route: this agent loop never knows whether it is the
    /// main session or a subagent. A route, if any, is added later by a
    /// forwarder relaying the event up from a nested dispatch.
    pub(crate) fn send_event(&self, event: StreamEvent) {
        if let Some(ref tx) = self.tx_events {
            let _ = tx.send(RoutedEvent::own(event));
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
            content: Some(Content::text(new_user_prompt)),
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

/// Build the outgoing `Content` for a turn's user message, mapping an
/// optional image attachment onto what `provider` actually accepts. Also
/// returns a notice message to show in the transcript, when the image had
/// to be dropped.
///
/// DeepSeek accepts no image content part at all: a hard 400 naming
/// `image_url` as an unknown variant, confirmed against the live API (see
/// `docs/notes/image-support.md`). So an image attachment on a DeepSeek
/// turn is dropped before the request is ever built, and the notice fires
/// up front rather than waiting on a 400 that would only confirm what is
/// already known. Ollama accepts the same OpenAI `image_url` shape on a
/// vision model; a non-vision model's own 400 comes back later, through
/// the ordinary stream-error path `run_turn` already has, not from here.
///
/// A `None` image leaves the returned `Content` exactly what plain text
/// already produced, on either provider: a turn with no attachment is
/// unaffected.
fn build_user_content(
    provider: Provider,
    text: &str,
    image: Option<&ImageAttachment>,
) -> (Content, Option<String>) {
    let Some(image) = image else {
        return (Content::text(text), None);
    };
    match provider {
        Provider::DeepSeek => (
            Content::text(text),
            Some(
                "DeepSeek does not support image attachments; the image was not sent."
                    .to_string(),
            ),
        ),
        Provider::Ollama => (
            Content::Parts(vec![
                ContentPart::Text { text: text.to_string() },
                ContentPart::ImageUrl {
                    url: format!("data:{};base64,{}", image.media_type, image.data),
                },
            ]),
            None,
        ),
    }
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
        // New tool call - ensure function and arguments exist
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
                image: None,
            })
        }
    }

    #[test]
    fn agent_config_defaults() {
        let cfg = AgentConfig::default();
        assert_eq!(cfg.max_turns, 100);
        assert_eq!(cfg.model, "deepseek-v4-flash");
        assert_eq!(cfg.effort, Effort::None);
    }

    #[test]
    fn agent_loop_creates_with_history() {
        let client = ApiClient::new(crate::api::client::Provider::DeepSeek, "sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let agent = AgentLoop::new(
            client,
            tools,
            "test prompt".into(),
            AgentConfig::default(),
            Arc::new(AtomicBool::new(false)),
        );
        assert_eq!(agent.history().len(), 0);
    }

    #[test]
    fn session_reset_clears_history() {
        let client = ApiClient::new(crate::api::client::Provider::DeepSeek, "sk-test".into(), None, None);
        let mut tools = ToolRegistry::new();
        tools.register(Arc::new(EchoTool));
        let mut agent = AgentLoop::new(
            client,
            tools,
            "initial".into(),
            AgentConfig::default(),
            Arc::new(AtomicBool::new(false)),
        );

        // Push a user message then reset
        agent.history.push(Message::user("hello".into()));
        assert_eq!(agent.history().len(), 1);

        agent.reset("fresh prompt".into(), "restart".into());
        // After reset: fresh system + 1 user message
        assert_eq!(agent.history().len(), 1);
    }

    #[test]
    fn interrupt_flag_is_the_injected_arc_not_a_fresh_one() {
        let client = ApiClient::new(crate::api::client::Provider::DeepSeek, "sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let gui_flag = Arc::new(AtomicBool::new(false));
        let agent = AgentLoop::new(
            client,
            tools,
            "test".into(),
            AgentConfig::default(),
            Arc::clone(&gui_flag),
        );

        // Set the flag through the handle the GUI would hold, never
        // through the agent's own getter, then check the agent's flag
        // reads true. That only happens if both point at the same
        // underlying `AtomicBool`.
        gui_flag.store(true, std::sync::atomic::Ordering::SeqCst);

        assert!(
            agent
                .interrupt_flag()
                .load(std::sync::atomic::Ordering::SeqCst)
        );
    }

    #[test]
    fn voice_mode_flag_defaults_to_false() {
        let client = ApiClient::new(crate::api::client::Provider::DeepSeek, "sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let agent = AgentLoop::new(
            client,
            tools,
            "test".into(),
            AgentConfig::default(),
            Arc::new(AtomicBool::new(false)),
        );
        assert!(
            !agent
                .voice_mode_flag()
                .load(std::sync::atomic::Ordering::SeqCst)
        );
    }

    #[test]
    fn voice_mode_flag_handle_observes_writes() {
        let client = ApiClient::new(crate::api::client::Provider::DeepSeek, "sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let agent = AgentLoop::new(
            client,
            tools,
            "test".into(),
            AgentConfig::default(),
            Arc::new(AtomicBool::new(false)),
        );
        let flag = agent.voice_mode_flag();
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(
            agent
                .voice_mode_flag()
                .load(std::sync::atomic::Ordering::SeqCst)
        );
    }

    #[test]
    fn effort_flag_defaults_to_none() {
        let client = ApiClient::new(crate::api::client::Provider::DeepSeek, "sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let agent = AgentLoop::new(
            client,
            tools,
            "test".into(),
            AgentConfig::default(),
            Arc::new(AtomicBool::new(false)),
        );
        assert_eq!(Effort::load(&agent.effort_flag()), Effort::None);
    }

    #[test]
    fn sync_dynamic_config_reads_the_effort_flag() {
        let client = ApiClient::new(crate::api::client::Provider::DeepSeek, "sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let mut agent = AgentLoop::new(
            client,
            tools,
            "sys prompt".into(),
            AgentConfig::default(),
            Arc::new(AtomicBool::new(false)),
        );
        Effort::Max.store(&agent.effort_flag());

        agent.sync_dynamic_config();

        assert_eq!(agent.config.effort, Effort::Max);
    }

    #[test]
    fn sync_dynamic_config_sets_voice_suffix_when_flag_true() {
        let client = ApiClient::new(crate::api::client::Provider::DeepSeek, "sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let mut agent = AgentLoop::new(
            client,
            tools,
            "sys prompt".into(),
            AgentConfig::default(),
            Arc::new(AtomicBool::new(false)),
        );
        agent
            .voice_mode_flag()
            .store(true, std::sync::atomic::Ordering::SeqCst);

        agent.sync_dynamic_config();

        let api = agent.history().to_api_messages();
        assert!(
            api[0]
                .content
                .as_ref()
                .and_then(Content::as_text)
                .unwrap()
                .contains("## Voice reply mode")
        );
    }

    #[test]
    fn sync_dynamic_config_clears_voice_suffix_when_flag_false() {
        let client = ApiClient::new(crate::api::client::Provider::DeepSeek, "sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let mut agent = AgentLoop::new(
            client,
            tools,
            "sys prompt".into(),
            AgentConfig::default(),
            Arc::new(AtomicBool::new(false)),
        );
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
                .as_ref()
                .and_then(Content::as_text)
                .unwrap()
                .contains("## Voice reply mode")
        );
    }

    #[test]
    fn sync_dynamic_config_reports_working_dir_when_set() {
        let client = ApiClient::new(crate::api::client::Provider::DeepSeek, "sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let mut agent = AgentLoop::new(
            client,
            tools,
            "sys prompt".into(),
            AgentConfig::default(),
            Arc::new(AtomicBool::new(false)),
        );
        let working_dir = Arc::new(Mutex::new(PathBuf::from("C:\\proj")));
        agent.set_working_dir(Arc::clone(&working_dir));

        agent.sync_dynamic_config();

        let api = agent.history().to_api_messages();
        assert!(
            api[0]
                .content
                .as_ref()
                .and_then(Content::as_text)
                .unwrap()
                .contains("Working directory: C:\\proj.")
        );
    }

    #[test]
    fn sync_dynamic_config_omits_working_dir_when_never_set() {
        let client = ApiClient::new(crate::api::client::Provider::DeepSeek, "sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let mut agent = AgentLoop::new(
            client,
            tools,
            "sys prompt".into(),
            AgentConfig::default(),
            Arc::new(AtomicBool::new(false)),
        );

        agent.sync_dynamic_config();

        let api = agent.history().to_api_messages();
        assert_eq!(api[0].content.as_ref().and_then(Content::as_text), Some("sys prompt"));
    }

    #[test]
    fn changing_shared_working_dir_between_turns_changes_next_syncs_prompt() {
        let client = ApiClient::new(crate::api::client::Provider::DeepSeek, "sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let mut agent = AgentLoop::new(
            client,
            tools,
            "sys prompt".into(),
            AgentConfig::default(),
            Arc::new(AtomicBool::new(false)),
        );
        let working_dir = Arc::new(Mutex::new(PathBuf::from("C:\\proj")));
        agent.set_working_dir(Arc::clone(&working_dir));

        agent.sync_dynamic_config();
        let first = agent.history().to_api_messages();
        assert!(first[0].content.as_ref().and_then(Content::as_text).unwrap().contains("C:\\proj"));

        // Change the shared value the way a future Cd tool or GUI control
        // would, with no restart and no re-registering of the agent.
        *working_dir.lock().unwrap() = PathBuf::from("C:\\other");
        agent.sync_dynamic_config();
        let second = agent.history().to_api_messages();
        assert!(second[0].content.as_ref().and_then(Content::as_text).unwrap().contains("C:\\other"));
        assert!(!second[0].content.as_ref().and_then(Content::as_text).unwrap().contains("C:\\proj"));
    }

    #[test]
    fn clear_history_then_sync_still_reports_the_current_directory() {
        let client = ApiClient::new(crate::api::client::Provider::DeepSeek, "sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let mut agent = AgentLoop::new(
            client,
            tools,
            "sys prompt".into(),
            AgentConfig::default(),
            Arc::new(AtomicBool::new(false)),
        );
        let working_dir = Arc::new(Mutex::new(PathBuf::from("C:\\proj")));
        agent.set_working_dir(Arc::clone(&working_dir));
        agent.sync_dynamic_config();
        agent.history.push(Message::user("hello".into()));

        agent.clear_history();
        // Rebuilt history has dropped the working directory line, exactly
        // like it drops the voice suffix, until the next sync restores it.
        assert_eq!(agent.history().len(), 0);

        agent.sync_dynamic_config();
        let api = agent.history().to_api_messages();
        assert!(api[0].content.as_ref().and_then(Content::as_text).unwrap().contains("C:\\proj"));
    }

    #[test]
    fn working_dir_and_voice_suffix_both_survive_together() {
        let client = ApiClient::new(crate::api::client::Provider::DeepSeek, "sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let mut agent = AgentLoop::new(
            client,
            tools,
            "sys prompt".into(),
            AgentConfig::default(),
            Arc::new(AtomicBool::new(false)),
        );
        let working_dir = Arc::new(Mutex::new(PathBuf::from("C:\\proj")));
        agent.set_working_dir(working_dir);
        agent
            .voice_mode_flag()
            .store(true, std::sync::atomic::Ordering::SeqCst);

        agent.sync_dynamic_config();

        let api = agent.history().to_api_messages();
        let content = api[0].content.as_ref().and_then(Content::as_text).unwrap();
        assert!(content.contains("Working directory: C:\\proj."));
        assert!(content.contains("## Voice reply mode"));
    }

    #[test]
    fn context_low_water_is_a_third_of_budget() {
        assert_eq!(context_low_water(100_000), 33_333);
        assert_eq!(context_low_water(300), 100);
    }

    #[test]
    fn context_budget_flag_defaults_to_100000() {
        let client = ApiClient::new(crate::api::client::Provider::DeepSeek, "sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let agent = AgentLoop::new(
            client,
            tools,
            "test".into(),
            AgentConfig::default(),
            Arc::new(AtomicBool::new(false)),
        );
        assert_eq!(
            agent
                .context_budget_flag()
                .load(std::sync::atomic::Ordering::SeqCst),
            DEFAULT_CONTEXT_BUDGET
        );
    }

    #[test]
    fn context_budget_flag_handle_observes_writes() {
        let client = ApiClient::new(crate::api::client::Provider::DeepSeek, "sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let agent = AgentLoop::new(
            client,
            tools,
            "test".into(),
            AgentConfig::default(),
            Arc::new(AtomicBool::new(false)),
        );
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
        let client = ApiClient::new(crate::api::client::Provider::DeepSeek, "sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let mut agent = AgentLoop::new(
            client,
            tools,
            "sys".into(),
            AgentConfig::default(),
            Arc::new(AtomicBool::new(false)),
        );
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
        let client = ApiClient::new(crate::api::client::Provider::DeepSeek, "sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let mut agent = AgentLoop::new(
            client,
            tools,
            "sys".into(),
            AgentConfig::default(),
            Arc::new(AtomicBool::new(false)),
        );

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
        let client = ApiClient::new(crate::api::client::Provider::DeepSeek, "sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let mut agent = AgentLoop::new(
            client,
            tools,
            "sys".into(),
            AgentConfig::default(),
            Arc::new(AtomicBool::new(false)),
        );
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
        let client = ApiClient::new(crate::api::client::Provider::DeepSeek, "sk-test".into(), None, None);
        let mut tools = ToolRegistry::new();
        tools.register(Arc::new(EchoTool));
        let mut agent = AgentLoop::new(
            client,
            tools,
            "initial".into(),
            AgentConfig::default(),
            Arc::new(AtomicBool::new(false)),
        );
        agent.history.push(Message::user("hi".into()));

        agent.rebuild_system_prompt(Some("memory"), Some("skills"));
        assert_eq!(agent.history().len(), 0);
    }

    #[tokio::test]
    async fn execute_tool_returns_output() {
        let client = ApiClient::new(crate::api::client::Provider::DeepSeek, "sk-test".into(), None, None);
        let mut tools = ToolRegistry::new();
        tools.register(Arc::new(EchoTool));
        let agent = AgentLoop::new(
            client,
            tools,
            "test".into(),
            AgentConfig::default(),
            Arc::new(AtomicBool::new(false)),
        );

        let output = agent.execute_tool("echo", "{}").await;
        assert!(!output.is_error);
        assert_eq!(output.content, "echoed");
    }

    #[tokio::test]
    async fn execute_unknown_tool_returns_error() {
        let client = ApiClient::new(crate::api::client::Provider::DeepSeek, "sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let agent = AgentLoop::new(
            client,
            tools,
            "test".into(),
            AgentConfig::default(),
            Arc::new(AtomicBool::new(false)),
        );

        let output = agent.execute_tool("nonexistent", "{}").await;
        assert!(output.is_error);
        assert!(output.content.contains("Unknown tool"));
    }

    fn sample_image() -> ImageAttachment {
        ImageAttachment {
            data: "AAA".into(),
            media_type: "image/png".into(),
        }
    }

    #[test]
    fn build_user_content_with_no_image_is_plain_text_on_either_provider() {
        for provider in [crate::api::client::Provider::DeepSeek, crate::api::client::Provider::Ollama] {
            let (content, notice) = build_user_content(provider, "hello", None);
            assert_eq!(content, Content::text("hello"));
            assert!(notice.is_none(), "provider {provider:?}");
        }
    }

    #[test]
    fn build_user_content_drops_the_image_on_deepseek_and_names_it_in_the_notice() {
        let image = sample_image();
        let (content, notice) =
            build_user_content(crate::api::client::Provider::DeepSeek, "look at this", Some(&image));

        assert_eq!(content, Content::text("look at this"));
        let notice = notice.expect("expected a notice for a DeepSeek image attachment");
        assert!(notice.contains("DeepSeek"), "notice should name the backend: {notice}");
    }

    #[test]
    fn build_user_content_maps_the_image_to_an_image_url_part_on_ollama() {
        let image = sample_image();
        let (content, notice) =
            build_user_content(crate::api::client::Provider::Ollama, "look at this", Some(&image));

        assert!(notice.is_none());
        assert_eq!(
            content,
            Content::Parts(vec![
                ContentPart::Text {
                    text: "look at this".into(),
                },
                ContentPart::ImageUrl {
                    url: "data:image/png;base64,AAA".into(),
                },
            ])
        );
    }

    /// Integration test: spawn a mock HTTP server returning SSE with reasoning_content,
    /// run the full agent loop, and verify Reasoning events are emitted.
    #[tokio::test]
    async fn effort_above_none_emits_reasoning_events() {
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
        let client = ApiClient::new(
            crate::api::client::Provider::DeepSeek,
            "sk-test".into(),
            Some(format!("http://127.0.0.1:{port}")),
            Some("deepseek-v4-flash".into()),
        );

        let tools = ToolRegistry::new();
        let mut config = AgentConfig::default();
        config.effort = Effort::High;

        let mut agent = AgentLoop::new(
            client,
            tools,
            "sys".into(),
            config,
            Arc::new(AtomicBool::new(false)),
        );

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
            events.push(ev.event);
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

    /// Build a stub subagent session ready to register into a
    /// `SubagentRegistry`. Its own script never matters here: these tests
    /// only check whether the session is still registered afterward, not
    /// what it would have answered.
    fn stub_session() -> crate::backend::Backend {
        crate::backend::Backend::Stub(Box::new(crate::backend::stub::StubBackend::new(
            Vec::new(),
            "stub-model".to_string(),
            Arc::new(AtomicBool::new(false)),
        )))
    }

    /// A minimal mock server that answers one turn with plain text and no
    /// tool calls, so `run` completes on its first attempt with no retry
    /// delay. Returns the port it bound to.
    fn spawn_text_only_mock_server() -> u16 {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let response_body = concat!(
            "data: {\"id\":\"t1\",\"object\":\"chat.completion.chunk\",\"created\":1,",
            "\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":",
            "{\"content\":\"hi\"},",
            "\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"t1\",\"object\":\"chat.completion.chunk\",\"created\":1,",
            "\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{},",
            "\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        )
        .to_string();

        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let http_response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}",
                response_body.len(),
                response_body,
            );
            let _ = stream.write_all(http_response.as_bytes());
            let _ = stream.flush();
            thread::sleep(std::time::Duration::from_millis(200));
        });

        port
    }

    /// The lifetime rule from the roadmap's Phase 3: a subagent session
    /// registered against the agent's registry does not survive the turn
    /// that opened it. `run` wraps `run_turn` specifically to guarantee
    /// this regardless of how the turn finishes.
    #[tokio::test]
    async fn no_session_survives_a_parent_turn_end() {
        let port = spawn_text_only_mock_server();
        let client = ApiClient::new(
            crate::api::client::Provider::DeepSeek,
            "sk-test".into(),
            Some(format!("http://127.0.0.1:{port}")),
            Some("deepseek-v4-flash".into()),
        );
        let tools = ToolRegistry::new();
        let mut agent = AgentLoop::new(
            client,
            tools,
            "sys".into(),
            AgentConfig::default(),
            Arc::new(AtomicBool::new(false)),
        );

        let registry = Arc::new(crate::backend::registry::SubagentRegistry::new());
        agent.set_subagent_registry(Arc::clone(&registry));
        let id = SubagentId::next();
        registry.register(id, stub_session()).await;
        assert!(registry.contains(id).await, "session should be live before the turn runs");

        let result = agent.run("hello").await;

        assert!(result.is_ok(), "turn should succeed: {:?}", result.err());
        assert!(
            !registry.contains(id).await,
            "session should not survive the turn that opened it"
        );
        assert_eq!(registry.len().await, 0);
    }

    /// `Reset` closes every session, per the roadmap's Phase 3 lifetime
    /// rule, even though `ResetTool` itself never touches the registry: the
    /// close happens in `execute_tool`'s `SessionReset` branch.
    #[tokio::test]
    async fn no_session_survives_a_reset() {
        let client = ApiClient::new(crate::api::client::Provider::DeepSeek, "sk-test".into(), None, None);
        let mut tools = ToolRegistry::new();
        tools.register(Arc::new(crate::tools::reset::ResetTool));
        let mut agent = AgentLoop::new(
            client,
            tools,
            "sys".into(),
            AgentConfig::default(),
            Arc::new(AtomicBool::new(false)),
        );

        let registry = Arc::new(crate::backend::registry::SubagentRegistry::new());
        agent.set_subagent_registry(Arc::clone(&registry));
        let id = SubagentId::next();
        registry.register(id, stub_session()).await;

        let output = agent
            .execute_tool("reset", "{\"prompt\":\"start fresh\"}")
            .await;

        assert!(!output.is_error);
        assert!(
            !registry.contains(id).await,
            "session should not survive a reset"
        );
        assert_eq!(registry.len().await, 0);
    }

    #[test]
    fn clear_history_drops_messages_keeps_system_prompt() {
        let client = ApiClient::new(crate::api::client::Provider::DeepSeek, "sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let mut agent = AgentLoop::new(
            client,
            tools,
            "sys prompt".into(),
            AgentConfig::default(),
            Arc::new(AtomicBool::new(false)),
        );
        agent.history.push(Message::user("hello".into()));
        assert_eq!(agent.history().len(), 1);

        agent.clear_history();

        assert_eq!(agent.history().len(), 0);
        let api = agent.history().to_api_messages();
        assert_eq!(api.len(), 1);
        assert_eq!(api[0].content.as_ref().and_then(Content::as_text), Some("sys prompt"));
    }

    #[test]
    fn restore_history_replaces_messages_and_keeps_system_prompt() {
        let client = ApiClient::new(crate::api::client::Provider::DeepSeek, "sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let mut agent = AgentLoop::new(
            client,
            tools,
            "sys prompt".into(),
            AgentConfig::default(),
            Arc::new(AtomicBool::new(false)),
        );
        agent.history.push(Message::user("stale".into()));

        agent.restore_history(vec![
            Message::user("saved one".into()),
            Message::assistant("saved reply".into()),
        ]);

        assert_eq!(agent.history().len(), 2);
        let api = agent.history().to_api_messages();
        assert_eq!(api[0].content.as_ref().and_then(Content::as_text), Some("sys prompt"));
    }

    #[test]
    fn clear_history_after_voice_suffix_still_produces_working_system_message() {
        let client = ApiClient::new(crate::api::client::Provider::DeepSeek, "sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let mut agent = AgentLoop::new(
            client,
            tools,
            "sys prompt".into(),
            AgentConfig::default(),
            Arc::new(AtomicBool::new(false)),
        );
        agent
            .voice_mode_flag()
            .store(true, std::sync::atomic::Ordering::SeqCst);
        agent.sync_dynamic_config();
        agent.history.push(Message::user("hello".into()));

        agent.clear_history();

        assert_eq!(agent.history().len(), 0);
        let api = agent.history().to_api_messages();
        assert_eq!(api.len(), 1);
        assert_eq!(api[0].content.as_ref().and_then(Content::as_text), Some("sys prompt"));

        // sync_dynamic_config still works after clear_history and restores
        // the voice suffix on the next turn.
        agent.sync_dynamic_config();
        let api = agent.history().to_api_messages();
        assert!(
            api[0]
                .content
                .as_ref()
                .and_then(Content::as_text)
                .unwrap()
                .contains("## Voice reply mode")
        );
    }

    #[tokio::test]
    async fn run_repeat_zero_iterations_emits_only_repeat_finished() {
        let client = ApiClient::new(crate::api::client::Provider::DeepSeek, "sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let mut agent = AgentLoop::new(
            client,
            tools,
            "sys".into(),
            AgentConfig::default(),
            Arc::new(AtomicBool::new(false)),
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        agent.set_event_sender(tx);

        super::super::repeat::run_repeat(&mut agent, "do the thing", 0).await;

        let mut events: Vec<StreamEvent> = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev.event);
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
        let client = ApiClient::new(crate::api::client::Provider::DeepSeek, "sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let mut agent = AgentLoop::new(
            client,
            tools,
            "sys".into(),
            AgentConfig::default(),
            Arc::new(AtomicBool::new(false)),
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        agent.set_event_sender(tx);
        agent
            .repeat_interrupt_flag()
            .store(true, std::sync::atomic::Ordering::SeqCst);

        super::super::repeat::run_repeat(&mut agent, "do the thing", 3).await;

        let mut events: Vec<StreamEvent> = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev.event);
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

        let client = ApiClient::new(
            crate::api::client::Provider::DeepSeek,
            "sk-test".into(),
            Some(format!("http://127.0.0.1:{port}")),
            Some("deepseek-v4-flash".into()),
        );

        let tools = ToolRegistry::new();
        let mut agent = AgentLoop::new(
            client,
            tools,
            "sys".into(),
            AgentConfig::default(),
            Arc::new(AtomicBool::new(false)),
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        agent.set_event_sender(tx);

        super::super::repeat::run_repeat(&mut agent, "do the task", 2).await;

        let mut events: Vec<StreamEvent> = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev.event);
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
        assert_eq!(last_assistant.content.as_ref().and_then(Content::as_text), Some("answer 1"));
    }
}
