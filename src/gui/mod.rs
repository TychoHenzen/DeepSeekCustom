pub(crate) mod attachment;
pub(crate) mod autopilot_tab;
pub(crate) mod session_state;
pub(crate) mod sessions_tab;
pub(crate) mod settings_panel;
pub(crate) mod transcript;
pub(crate) mod voice_ui;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::App;
use eframe::egui::{self, Color32, RichText, ScrollArea, TextEdit};
use egui_commonmark::CommonMarkCache;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use self::attachment::{AttachmentSlot, decode_image_bytes};
use self::autopilot_tab::AutopilotTab;
use self::session_state::{SessionOrigin, SessionState};
use self::settings_panel::spawn_model_list_fetch;
use self::transcript::{Block, BlockId, BlockKind, Severity, Span, SubagentState, Transcript};
use self::voice_ui::{PttKeys, VoiceUi, voice_state_color, voice_state_label};
use crate::agent::agent_loop::{AgentCommand, RoutedEvent, StreamEvent};
use crate::agent::repeat::RepeatCommand;
use crate::api::types::ImageAttachment;
use crate::config::settings::Settings;
use crate::effort::Effort;
use crate::session::{SessionId, SessionStore};
use crate::voice::service::{VoiceCommand, VoiceEvent};

/// Which tab the main window shows. Chat is the default; Autopilot is a
/// dedicated tab for running one task repeatedly with automatic question
/// answering, added alongside the existing settings sidebar (Tab key).
/// Sessions lists saved conversations and lets the user start, reopen, or
/// delete one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum ActiveTab {
    #[default]
    Chat,
    Autopilot,
    Sessions,
}

/// Native GUI using egui/eframe. Replaces the broken ratatui TUI.
pub struct DeepSeekGui {
    /// The Chat tab's history, as structured blocks. Replaces the flat
    /// list of coloured lines this field used to hold: a block carries
    /// what a line's colour used to imply, and a tool call's output can
    /// be filled in after its block was appended.
    transcript: Transcript,
    input_buffer: String,
    model: String,
    token_count: String,
    session_status: String,
    /// Cumulative prompt cache hit tokens (not charged as input).
    total_cache_hit_tokens: u32,
    /// Cumulative prompt cache miss tokens (charged as input).
    total_cache_miss_tokens: u32,
    rx_events: mpsc::UnboundedReceiver<RoutedEvent>,
    tx_input: mpsc::UnboundedSender<AgentCommand>,
    auto_scroll: bool,

    // ── Interrupt ──
    /// Flag shared with agent loop; set on Escape to abort streaming.
    interrupt_flag: Arc<AtomicBool>,

    // ── Settings panel ──
    settings_visible: bool,
    /// Sorted backend names, the keys of `settings.backends()`.
    backend_options: Vec<String>,
    selected_backend_idx: usize,
    /// The backend the running session was actually built on, fixed at
    /// startup. The picker may point somewhere else, since a backend
    /// switch only takes effect on the next start. A model change writes
    /// the shared `model_flag` only while the two still agree: sending
    /// another backend's model name to the running one would break the
    /// next turn.
    active_backend: Option<String>,
    /// Options for the model dropdown, resolved for the selected backend.
    /// Seeded synchronously with that backend's declared model, so the
    /// dropdown is never empty. Replaced once the background fetch in
    /// `spawn_model_list_fetch` completes.
    model_options: Vec<String>,
    /// Background model-discovery results, tagged with the backend name
    /// they were resolved for. `update()` drains this each frame. A
    /// result tagged for a backend that is no longer selected is dropped.
    model_list_rx: mpsc::UnboundedReceiver<(String, Vec<String>)>,
    /// Sender half of `model_list_rx`. Cloned into each background fetch
    /// spawned by `spawn_model_list_fetch`.
    model_list_tx: mpsc::UnboundedSender<(String, Vec<String>)>,

    // ── Shared state with agent ──
    effort_flag: Arc<AtomicU8>,
    /// Sidebar combo box's current selection, seeded from `effort_flag` in
    /// `new`. Mirrors `context_budget`: the flag is what the agent reads
    /// each turn, this field is what the control renders and edits.
    effort: Effort,
    model_flag: Arc<Mutex<String>>,
    /// Flag shared with agent loop. Kept in sync with `voice_tts_enabled` so
    /// the agent knows to shape replies for speech while text to speech is on.
    voice_mode_flag: Arc<AtomicBool>,
    /// Flag shared with agent loop, holding the context budget in tokens.
    context_budget_flag: Arc<AtomicUsize>,
    /// Slider's current value, seeded from `context_budget_flag` in `new`.
    context_budget: usize,
    /// Shared with the agent and with `Bash`, `Read`, `Write`, and `Cd`.
    /// Where those tools act, distinct from `project_root`, which never
    /// moves. A write here takes effect on the next tool call and the next
    /// turn's system prompt.
    working_dir_flag: Arc<Mutex<PathBuf>>,
    /// Sidebar text field's buffer, seeded from `working_dir_flag` in
    /// `new`. Only a valid, existing directory is written back to
    /// `working_dir_flag`; an invalid entry stays in the buffer for the
    /// user to see and correct, without touching the shared handle.
    working_dir_buffer: String,

    // ── Output display ──
    show_raw_output: bool,
    markdown_cache: CommonMarkCache,

    // Voice: the two channels, the sidebar's controls, and the reply
    // waiting to be spoken. See `gui::voice_ui`.
    voice: VoiceUi,

    // ── Settings persistence ──
    /// The settings this GUI writes back on every control change. Seeded
    /// from the settings loaded at startup.
    settings: Settings,
    /// Directory holding `settings.json`, the file `persist_settings`
    /// writes.
    project_root: PathBuf,

    // ── Autopilot (optional, wired by `with_repeat`) ──
    /// Sends a repeat command to the agent task. `None` until `with_repeat`
    // Autopilot: the task, the iteration count, the policy path, the
    // progress readout, and the two channels a run needs. See
    // `gui::autopilot_tab`.
    autopilot: AutopilotTab,

    /// Which tab the main window shows. Defaults to Chat.
    active_tab: ActiveTab,

    // ── Session state (S08): saved conversations, no UI yet ──
    // Saved conversations: the current id and metadata, the saved list,
    // and the API history a save needs. See `gui::session_state`.
    sessions: SessionState,

    // ── Pending image attachment (P6S05) ──
    /// The image the next turn will carry, the OS clipboard handle, and
    /// the four input paths that fill the slot. See `gui::attachment`.
    attachment: AttachmentSlot,
}

impl DeepSeekGui {
    pub fn new(
        rx_events: mpsc::UnboundedReceiver<RoutedEvent>,
        tx_input: mpsc::UnboundedSender<AgentCommand>,
        interrupt_flag: Arc<AtomicBool>,
        effort_flag: Arc<AtomicU8>,
        voice_mode_flag: Arc<AtomicBool>,
        context_budget_flag: Arc<AtomicUsize>,
        model_flag: Arc<Mutex<String>>,
        working_dir_flag: Arc<Mutex<PathBuf>>,
        settings: Settings,
        project_root: PathBuf,
    ) -> Self {
        let mut backend_options: Vec<String> = settings
            .backends()
            .map(|b| b.keys().cloned().collect())
            .unwrap_or_default();
        backend_options.sort();
        let selected_backend_idx = settings
            .default_backend()
            .and_then(|name| backend_options.iter().position(|b| b == name))
            .unwrap_or(0);
        let current_model = model_flag.lock().unwrap().clone();
        let (model_list_tx, model_list_rx) = mpsc::unbounded_channel::<(String, Vec<String>)>();
        let model_options = vec![current_model.clone()];
        if let Some(name) = backend_options.get(selected_backend_idx) {
            if let Some(cfg) = settings.resolve_backend(name) {
                spawn_model_list_fetch(model_list_tx.clone(), name.clone(), cfg.clone());
            }
        }
        let voice = VoiceUi::new(&settings, &voice_mode_flag);
        let context_budget = context_budget_flag.load(Ordering::SeqCst);
        let effort = Effort::load(&effort_flag);
        // Seed the text field from the shared handle, which `main.rs`
        // already resolved against a saved `working_dir` setting (falling
        // back to `project_root` when that setting was absent or no longer
        // a real directory). Reading it back here, rather than reading
        // `settings.working_dir()` a second time, keeps this single source
        // of truth: the buffer always starts equal to what the tools will
        // actually act against.
        let working_dir_buffer = working_dir_flag.lock().unwrap().display().to_string();
        let autopilot = AutopilotTab::new(&settings, &project_root);
        let session_backend = backend_options
            .get(selected_backend_idx)
            .cloned()
            .unwrap_or_default();
        let sessions = SessionState::new(
            SessionStore::for_project(&project_root),
            SessionOrigin {
                backend: session_backend,
                model: current_model.clone(),
            },
        );
        Self {
            transcript: Transcript::new(),
            input_buffer: String::new(),
            model: current_model,
            token_count: "0".into(),
            session_status: "Ready".into(),
            total_cache_hit_tokens: 0,
            total_cache_miss_tokens: 0,
            rx_events,
            tx_input,
            auto_scroll: false,
            interrupt_flag,
            settings_visible: false,
            active_backend: backend_options.get(selected_backend_idx).cloned(),
            backend_options,
            selected_backend_idx,
            model_options,
            model_list_rx,
            model_list_tx,
            effort_flag,
            effort,
            voice_mode_flag,
            context_budget_flag,
            context_budget,
            model_flag,
            working_dir_flag,
            working_dir_buffer,
            show_raw_output: settings.show_raw_output(),
            markdown_cache: CommonMarkCache::default(),
            voice,
            settings,
            project_root,
            autopilot,
            active_tab: ActiveTab::default(),
            sessions,
            attachment: AttachmentSlot::new(),
        }
    }

    /// Attach the repeat command sender and the agent's repeat interrupt
    /// flag. Called only from `main`, which owns both. Skipping this call
    /// leaves both `None`. The Autopilot tab in step S08 needs them. This
    /// step only wires the plumbing through.
    pub fn with_repeat(
        mut self,
        repeat_tx: mpsc::UnboundedSender<RepeatCommand>,
        repeat_interrupt_flag: Arc<AtomicBool>,
    ) -> Self {
        self.autopilot.attach(repeat_tx, repeat_interrupt_flag);
        self
    }

    /// Write the current settings to `<project_root>/settings.json`.
    /// Called by every settings-panel control after it updates
    /// `self.settings`. A save failure is logged and otherwise ignored:
    /// losing a preference must never take the session down.
    fn persist_settings(&self) {
        if let Err(e) = self.settings.save(&self.project_root) {
            warn!(error = %e, "failed to save settings.json");
        }
    }

    /// Attach the voice subsystem's event receiver and command sender.
    /// Called only when voice is enabled and its models resolved. Skipping
    /// this call leaves both `None` and the GUI behaves exactly as it did
    /// before voice support existed.
    pub fn with_voice(
        mut self,
        voice_rx: mpsc::UnboundedReceiver<VoiceEvent>,
        voice_tx: mpsc::UnboundedSender<VoiceCommand>,
    ) -> Self {
        self.voice.attach(voice_rx, voice_tx);
        self
    }

    /// Submit whatever is in `input_buffer` to the agent: append a `User`
    /// block to the transcript, mark the session running, forward the text
    /// down `tx_input`, clear the buffer. This is the single submission
    /// path both the Enter key and a voice transcript go through, so the
    /// agent cannot tell the two apart. No-ops on empty or whitespace-only
    /// input.
    ///
    /// The block holds the text the user typed, with no "> " prefix. The
    /// prefix was decoration the flat-line shape needed to tell user input
    /// apart from model output. `BlockKind::User` says that outright, and
    /// the renderer adds the marker back.
    fn submit_current_input(&mut self) {
        if self.input_buffer.trim().is_empty() && self.attachment.is_empty() {
            return;
        }
        let input = std::mem::take(&mut self.input_buffer);
        self.transcript.push(BlockKind::User {
            text: input.clone(),
        });
        // The slot holds at most one image (see `AttachmentSlot::set`),
        // so draining it hands over that single image, or `None`. Pushing
        // its own `Image` block here is what makes the sent image show up
        // in the transcript on the user's side, matching `render_image_block`
        // showing whatever an assistant or tool turn attaches on the other.
        let attachment = self.attachment.take();
        if let Some(image) = attachment.clone() {
            self.transcript.push(BlockKind::Image { image });
        }
        self.session_status = "Running...".into();
        let _ = self.tx_input.send(AgentCommand::UserTurn {
            text: input,
            image: attachment,
        });
        self.auto_scroll = true;
    }

    /// Apply one voice event exactly as the frame loop does. Test-only:
    /// every real event arrives through `drain_events` in `update`.
    #[cfg(test)]
    fn handle_voice_event_for_test(&mut self, event: VoiceEvent) {
        if let Some(text) = self.voice.handle_event(event, &mut self.transcript) {
            self.input_buffer = text;
            self.submit_current_input();
        }
    }

    /// Which backend and model a save should record. Built fresh on each
    /// call rather than held, since the model picker can move under it.
    fn session_origin(&self) -> SessionOrigin {
        SessionOrigin {
            backend: self.active_backend.clone().unwrap_or_default(),
            model: self.model.clone(),
        }
    }

    /// Save the current conversation to disk. Called after every `TurnEnd`.
    fn autosave_session(&mut self) {
        let origin = self.session_origin();
        self.sessions.autosave(&mut self.transcript, origin);
    }

    /// Start a fresh conversation and tell the agent to start over.
    fn start_new_session(&mut self) {
        let origin = self.session_origin();
        let command = self.sessions.start_new(&mut self.transcript, origin);
        let _ = self.tx_input.send(command);
    }

    /// Open a saved conversation and tell the agent to replay its history.
    fn load_session(&mut self, id: SessionId) {
        let origin = self.session_origin();
        if let Some(command) = self.sessions.load(id, &mut self.transcript, origin) {
            let _ = self.tx_input.send(command);
        }
    }

    /// Route one main-session stream event, exactly as if it arrived with
    /// an empty route. A test-only convenience: every real event from
    /// `rx_events` goes through `handle_routed_event` instead, since only
    /// that method can tell a main-session event apart from a subagent's.
    #[cfg(test)]
    fn handle_stream_event(&mut self, event: StreamEvent) {
        self.handle_routed_event(RoutedEvent::own(event));
    }

    /// Route one `RoutedEvent`. An empty route names a main-session event:
    /// it gets everything `apply_event_side_effects` does today, the same
    /// as before routing existed, then reaches the transcript. A non-empty
    /// route names a subagent's event: it is not this session's own turn,
    /// so none of the status bar counters, the speech buffer, the
    /// Autopilot progress readout, the session autosave, or any other
    /// side effect may fire for it. It only ever reaches the transcript,
    /// which resolves the route to the right nested block.
    fn handle_routed_event(&mut self, routed: RoutedEvent) {
        if routed.route.is_empty() {
            self.apply_event_side_effects(&routed.event);
        }
        self.transcript.apply_routed_event(routed);
    }

    /// Update everything a stream event touches other than the transcript.
    /// Split out from `handle_stream_event` so that method stays a plain
    /// two-step: side effects, then transcript.
    fn apply_event_side_effects(&mut self, event: &StreamEvent) {
        match event {
            StreamEvent::Text { text, .. } => self.voice.push_reply_text(text),
            StreamEvent::ToolCallStart { tool, args, .. } => {
                info!(tool=%tool, args=%args, "tool call start");
            }
            StreamEvent::ToolCallEnd {
                tool,
                output,
                is_error,
                ..
            } => log_tool_call_end(tool, output, *is_error),
            StreamEvent::ConversationSnapshot {
                messages,
                claude_session_id,
            } => self.sessions.record_snapshot(messages, claude_session_id),
            StreamEvent::TurnEnd {
                total_tokens,
                prompt_cache_hit_tokens,
                prompt_cache_miss_tokens,
                ..
            } => {
                self.token_count = total_tokens.to_string();
                self.total_cache_hit_tokens += prompt_cache_hit_tokens;
                self.total_cache_miss_tokens += prompt_cache_miss_tokens;
                self.voice.speak_accumulated_reply();
                self.autosave_session();
            }
            StreamEvent::SessionReset => self.reset_session_state(),
            StreamEvent::Error { message } => error!(%message, "stream error event"),
            StreamEvent::Interrupted { message } => {
                info!(%message, "agent interrupted");
                self.session_status = "Interrupted".into();
                // An interrupted turn never reaches TurnEnd, so nothing
                // would otherwise clear the partial reply gathered so
                // far. Drop it rather than folding it into the next
                // turn's speech.
                self.voice.clear_reply();
            }
            StreamEvent::RepeatIterationStart { index, total } => {
                info!(index, total, "repeat iteration start");
                self.autopilot.set_running(*index, *total);
            }
            StreamEvent::RepeatFinished { completed, total } => {
                info!(completed, total, "repeat run finished");
                self.autopilot.set_finished(*completed, *total);
            }
            StreamEvent::Reasoning { .. } => {}
        }
    }

    /// Handle a self-triggered session reset (the agent's `Reset` tool).
    /// Saves the outgoing conversation into a session record instead of
    /// discarding it, then starts a fresh one and leaves one notice
    /// behind. The transcript's own `SessionReset` arm is a no-op, so the
    /// clear belongs here, where the rest of the reset already lives.
    fn reset_session_state(&mut self) {
        let origin = self.session_origin();
        self.sessions
            .save_outgoing_and_start_new(&mut self.transcript, origin);
        self.transcript.push(BlockKind::Notice {
            text: "Session reset".into(),
            severity: Severity::Info,
        });
        self.session_status = "Reset".into();
        self.total_cache_hit_tokens = 0;
        self.total_cache_miss_tokens = 0;
        self.voice.clear_reply();
    }

    /// Render the Chat tab's output scroll area. Each block is drawn with
    /// the widget its kind calls for: a bubble for a message, a folding
    /// summary for a tool call, a single styled line for a notice.
    fn render_chat_output(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Output");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("Blocks: {}", self.transcript.blocks().len()))
                        .color(Color32::GRAY)
                        .small(),
                );
            });
        });
        ui.separator();
        // Move the transcript out of `self` for the duration of the draw.
        // A block renderer needs the blocks by shared reference and the
        // markdown cache by mutable reference at the same moment, which
        // it cannot have while both live on `self`.
        let transcript = std::mem::take(&mut self.transcript);
        let mut toggled: Vec<Vec<BlockId>> = Vec::new();
        let mut pin_toggled: Vec<Vec<BlockId>> = Vec::new();
        ScrollArea::vertical()
            .stick_to_bottom(self.auto_scroll)
            .show(ui, |ui| {
                if self.show_raw_output {
                    render_raw_blocks(ui, transcript.blocks());
                    return;
                }
                for (index, block) in transcript.blocks().iter().enumerate() {
                    ui.add_space(gap_before(index, &block.kind));
                    self.render_block(ui, block, &[], &mut toggled, &mut pin_toggled);
                }
            });
        self.transcript = transcript;
        // Applied after the draw, since the closure above only holds the
        // blocks by shared reference. A path may name a block nested
        // inside a `Subagent` block's own transcript, not just a
        // top-level one, so both go through the path-aware setters.
        for path in toggled {
            let collapsed = self
                .transcript
                .find_mut_by_path(&path)
                .is_some_and(|block| block.collapsed);
            self.transcript.set_collapsed_by_path(&path, !collapsed);
        }
        for path in pin_toggled {
            let pinned = self
                .transcript
                .find_mut_by_path(&path)
                .is_some_and(|block| block.pinned);
            self.transcript.set_pinned_by_path(&path, !pinned);
        }
    }

    /// Draw one block with the widget its kind calls for. Every clicked
    /// disclosure or pin toggle is recorded in `toggled` / `pin_toggled`
    /// instead of applied here. `path_prefix` names the chain of
    /// `Subagent` block ids this block is nested inside, outermost first,
    /// empty for a top-level block. The full path to `block` itself is
    /// `path_prefix` with `block.id` appended, and that full path is what
    /// gets recorded on a click, since a `BlockId` alone is only unique
    /// within the `Transcript` that owns it.
    fn render_block(
        &mut self,
        ui: &mut egui::Ui,
        block: &Block,
        path_prefix: &[BlockId],
        toggled: &mut Vec<Vec<BlockId>>,
        pin_toggled: &mut Vec<Vec<BlockId>>,
    ) {
        match &block.kind {
            BlockKind::User { text } => render_user_bubble(ui, role_label(&block.kind), text),
            BlockKind::Assistant { spans } => {
                self.render_assistant_bubble(ui, block, spans);
            }
            BlockKind::ToolCall { .. } => {
                let mut path = path_prefix.to_vec();
                path.push(block.id);
                render_tool_call(ui, block, &path, toggled);
            }
            BlockKind::Notice { text, severity } => {
                ui.label(RichText::new(text).color(severity_color(*severity)));
            }
            BlockKind::Image { image } => render_image_block(ui, block.id, image),
            BlockKind::Subagent {
                backend,
                model,
                depth,
                state,
                elapsed_ms,
                started_at,
                transcript,
                session_turns,
                session_turn_cap,
                send_message_calls,
                send_message_call_cap,
                ..
            } => {
                self.render_subagent_block(
                    ui,
                    block,
                    backend,
                    model,
                    *depth,
                    *state,
                    subagent_elapsed_ms(*started_at, *elapsed_ms),
                    *session_turns,
                    *session_turn_cap,
                    *send_message_calls,
                    *send_message_call_cap,
                    transcript,
                    path_prefix,
                    toggled,
                    pin_toggled,
                );
            }
        }
    }

    /// One `Subagent` block: a collapsing header carrying the backend,
    /// model, depth, state badge and elapsed time, plus a pin control.
    /// Collapsed by default; a running dispatch never auto-expands, since
    /// a fanout of several subagents must not push the main conversation
    /// off screen. Opening it renders the subagent's own inner transcript
    /// through this same method, indented, so a nested dispatch inside it
    /// draws exactly the way a top-level one does.
    #[allow(clippy::too_many_arguments)]
    fn render_subagent_block(
        &mut self,
        ui: &mut egui::Ui,
        block: &Block,
        backend: &str,
        model: &str,
        depth: u32,
        state: SubagentState,
        elapsed_ms: u64,
        session_turns: u32,
        session_turn_cap: u32,
        send_message_calls: u32,
        send_message_call_cap: u32,
        inner: &Transcript,
        path_prefix: &[BlockId],
        toggled: &mut Vec<Vec<BlockId>>,
        pin_toggled: &mut Vec<Vec<BlockId>>,
    ) {
        let mut path = path_prefix.to_vec();
        path.push(block.id);
        let open = block.pinned || !block.collapsed;
        let header_text = subagent_header_summary(
            backend,
            model,
            depth,
            state,
            elapsed_ms,
            session_turns,
            session_turn_cap,
            send_message_calls,
            send_message_call_cap,
        );
        ui.horizontal(|ui| {
            let pin_label = if block.pinned { "Unpin" } else { "Pin" };
            if ui.small_button(pin_label).clicked() {
                pin_toggled.push(path.clone());
            }
            let header = egui::CollapsingHeader::new(
                RichText::new(header_text).color(subagent_state_color(state)),
            )
            .id_salt(path.clone())
            .open(Some(open))
            .show(ui, |ui| {
                ui.indent(("subagent-body", path.clone()), |ui| {
                    for (index, inner_block) in inner.blocks().iter().enumerate() {
                        ui.add_space(gap_before(index, &inner_block.kind));
                        self.render_block(ui, inner_block, &path, toggled, pin_toggled);
                    }
                });
            });
            if header.header_response.clicked() {
                toggled.push(path.clone());
            }
        });
    }

    /// The assistant's reply as a bubble: a role label, then every span in
    /// arrival order.
    fn render_assistant_bubble(&mut self, ui: &mut egui::Ui, block: &Block, spans: &[Span]) {
        let label = role_label(&block.kind);
        let id = block.id;
        bubble_frame(ASSISTANT_BUBBLE_FILL).show(ui, |ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new(label).color(ASSISTANT_ROLE_COLOR).small());
                for (index, span) in spans.iter().enumerate() {
                    self.render_span(ui, id, index, span);
                }
            });
        });
    }

    /// One assistant span. Reply text goes through the markdown renderer.
    /// A reasoning span is dimmed and folded away by default, so a long
    /// chain of thought does not push the answer off screen.
    fn render_span(&mut self, ui: &mut egui::Ui, id: BlockId, index: usize, span: &Span) {
        match span {
            Span::Text(text) => {
                egui_commonmark::CommonMarkViewer::new().show(ui, &mut self.markdown_cache, text);
            }
            Span::Reasoning(text) => {
                egui::CollapsingHeader::new(
                    RichText::new(REASONING_LABEL)
                        .color(REASONING_COLOR)
                        .small(),
                )
                .id_salt((id, index))
                .default_open(false)
                .show(ui, |ui| {
                    ui.label(RichText::new(text).color(REASONING_COLOR));
                });
            }
        }
    }
}

impl App for DeepSeekGui {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Poll agent events each frame. `handle_routed_event` resolves the
        // route: an empty one behaves exactly as a main-session event
        // always did, a non-empty one lands inside the right nested
        // `Subagent` block instead of triggering any main-session side
        // effect.
        while let Ok(routed) = self.rx_events.try_recv() {
            self.handle_routed_event(routed);
            self.auto_scroll = true;
        }
        // Poll voice events each frame too, alongside agent events. Drain
        // into a buffer first so the mutable borrow of `voice_rx` ends
        // before `handle_voice_event` needs `&mut self`.
        for event in self.voice.drain_events() {
            // A finished transcript goes through the input buffer and
            // the same submit path Enter uses, so the agent cannot tell
            // a spoken turn from a typed one.
            if let Some(text) = self.voice.handle_event(event, &mut self.transcript) {
                self.input_buffer = text;
                self.submit_current_input();
            }
        }
        // Poll background model-list fetches each frame.
        while let Ok((backend_name, models)) = self.model_list_rx.try_recv() {
            self.apply_fetched_model_list(&backend_name, models);
        }
        // Keep polling at ~20fps even when no user input
        ctx.request_repaint_after(Duration::from_millis(50));

        // ── Settings panel (right side, Tab toggles) ──
        self.render_settings_panel(ctx);

        // ── Global keybindings ──
        //
        // Read once per frame, ahead of the tab content, so Escape, Tab,
        // Ctrl+Q, and push-to-talk keep working no matter which tab is
        // active. Only the chat text box's Enter-to-submit stays tied to
        // the Chat tab, since there is no message box to submit otherwise.
        let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        let tab_pressed = ctx.input(|i| i.key_pressed(egui::Key::Tab));
        let ctrl_q = ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Q));
        let ctrl_held = ctx.input(|i| i.modifiers.ctrl);
        let space_pressed = ctx.input(|i| i.key_pressed(egui::Key::Space));
        let space_released = ctx.input(|i| i.key_released(egui::Key::Space));
        let ctrl_space_pressed = ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Space));
        let any_widget_focused = ctx.memory(|mem| mem.focused().is_some());

        // Ctrl+V image paste and drag-and-drop, both feeding the pending
        // attachment strip. Neither is tied to the Chat tab's text box
        // having focus: a screenshot pasted while the settings panel is
        // open, or a file dropped anywhere on the window, still attaches.
        self.attachment.poll_ctrl_v_paste(ctx, &mut self.transcript);
        self.attachment
            .handle_dropped_files(ctx, &mut self.transcript);

        // Both push-to-talk bindings. The sidebar owns the keyboard
        // while it is open, so it closes off both.
        self.voice.handle_ptt(
            PttKeys {
                space_pressed,
                space_released,
                ctrl_held,
                ctrl_space_pressed,
                input_focused: any_widget_focused,
            },
            self.settings_visible,
        );

        // Escape - interrupt agent, stop any speech in progress, and stop
        // a running autopilot repeat. One Escape cuts off whatever the
        // session is doing, in any tab.
        if escape {
            info!("user pressed Escape - interrupting agent");
            self.interrupt_flag.store(true, Ordering::SeqCst);
            self.autopilot.request_stop();
            self.voice.send(VoiceCommand::StopSpeaking);
            self.transcript.push(BlockKind::Notice {
                text: "[Interrupting...]".into(),
                severity: Severity::Warning,
            });
        }

        // Tab -> toggle settings panel
        if tab_pressed {
            self.settings_visible = !self.settings_visible;
        }

        // Ctrl+Q -> quit
        if ctrl_q {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        // ── Tab bar and content (central panel) ──
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.active_tab, ActiveTab::Chat, "Chat");
                ui.selectable_value(&mut self.active_tab, ActiveTab::Autopilot, "Autopilot");
                ui.selectable_value(&mut self.active_tab, ActiveTab::Sessions, "Sessions");
            });
            ui.separator();
            match self.active_tab {
                ActiveTab::Chat => self.render_chat_output(ui),
                ActiveTab::Autopilot => {
                    if self.autopilot.render(ui, &mut self.settings) {
                        self.persist_settings();
                    }
                }
                ActiveTab::Sessions => self.render_sessions_tab(ui),
            }
        });
        self.auto_scroll = false;

        // ── Input bar (Chat tab only) ──
        if self.active_tab == ActiveTab::Chat {
            egui::TopBottomPanel::bottom("input_panel")
                .min_height(32.0)
                .show(ctx, |ui| {
                    self.attachment.render_strip(ui);
                    ui.horizontal(|ui| {
                        ui.label(">");
                        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));

                        let response = ui.add(
                            TextEdit::singleline(&mut self.input_buffer)
                                .hint_text("Type your message...")
                                .desired_width(f32::INFINITY),
                        );
                        // Auto-focus the input only when nothing else holds
                        // focus, instead of every frame. Forcing focus every
                        // frame would make `response.has_focus()` always
                        // true, and space-bar push-to-talk above would
                        // never be able to tell "typing" from "not typing".
                        if ctx.memory(|mem| mem.focused().is_none()) {
                            response.request_focus();
                        }

                        if enter {
                            self.submit_current_input();
                        }
                    });
                });
        }

        // ── Status bar ──
        egui::TopBottomPanel::bottom("status_bar")
            .min_height(24.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let backend_name = self
                        .backend_options
                        .get(self.selected_backend_idx)
                        .map(String::as_str)
                        .unwrap_or("unknown");
                    ui.label(
                        RichText::new(format!("Backend: {backend_name} ({})", self.model))
                            .color(Color32::from_rgb(0, 255, 255)),
                    );
                    ui.separator();
                    ui.label(
                        RichText::new(format!(
                            "Dir: {}",
                            self.working_dir_flag.lock().unwrap().display()
                        ))
                        .color(Color32::from_rgb(160, 160, 160)),
                    );
                    ui.separator();
                    ui.label(
                        RichText::new(format!("Tokens: {}", self.token_count))
                            .color(Color32::from_rgb(160, 160, 160)),
                    );
                    if self.total_cache_hit_tokens > 0 || self.total_cache_miss_tokens > 0 {
                        ui.separator();
                        let cache_info = format!(
                            "Cache hit: {} | miss: {}",
                            self.total_cache_hit_tokens, self.total_cache_miss_tokens
                        );
                        ui.label(
                            RichText::new(cache_info)
                                .color(Color32::from_rgb(100, 200, 100))
                                .small(),
                        );
                    }
                    ui.separator();
                    ui.label(
                        RichText::new(&self.session_status).color(Color32::from_rgb(0, 200, 0)),
                    );
                    ui.separator();
                    let effort_label = format!("Effort: {:?}", Effort::load(&self.effort_flag));
                    ui.label(
                        RichText::new(effort_label)
                            .color(Color32::from_rgb(200, 200, 100))
                            .small(),
                    );
                    ui.separator();
                    ui.label(
                        RichText::new(voice_state_label(self.voice.state()))
                            .color(voice_state_color(self.voice.state()))
                            .small(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new("Tab: settings | Esc: interrupt | Ctrl+Q: quit")
                                .color(Color32::from_rgb(128, 128, 128))
                                .small(),
                        );
                    });
                });
            });
    }
}

/// Log one finished tool call. Split out of the stream-event handler so
/// that method stays a flat match with one statement per arm.
fn log_tool_call_end(tool: &str, output: &str, is_error: bool) {
    if is_error {
        warn!(tool = %tool, error = %output, "tool call failed");
    } else {
        debug!(tool = %tool, "tool call ok");
    }
}

/// Colour and layout constants for the chat log. Colour is a rendering
/// choice made from a block's kind and severity. It carries no meaning of
/// its own, so every one of these decisions stays in this file.
const USER_BUBBLE_FILL: Color32 = Color32::from_rgb(38, 52, 74);
const ASSISTANT_BUBBLE_FILL: Color32 = Color32::from_rgb(38, 42, 48);
const USER_ROLE_COLOR: Color32 = Color32::from_rgb(140, 180, 240);
const ASSISTANT_ROLE_COLOR: Color32 = Color32::from_rgb(150, 200, 160);
const REASONING_COLOR: Color32 = Color32::from_rgb(140, 140, 140);
const TOOL_COLOR: Color32 = Color32::from_rgb(255, 255, 0);
const TOOL_ERROR_COLOR: Color32 = Color32::from_rgb(255, 80, 80);
const TOOL_OUTPUT_COLOR: Color32 = Color32::from_rgb(0, 200, 0);
const REASONING_LABEL: &str = "Reasoning";
const IMAGE_LABEL_COLOR: Color32 = Color32::from_rgb(200, 160, 220);
/// The longer side of an inline image thumbnail in the transcript, in
/// points. Clicking it opens the same image full size in its own window.
const IMAGE_THUMBNAIL_MAX: f32 = 240.0;
/// Space above a block that opens a new turn. This is what replaced the
/// old grey "--- turn end ---" divider: a turn boundary now reads as a
/// gap between bubbles instead of a line of its own.
const TURN_GAP: f32 = 14.0;
const BLOCK_GAP: f32 = 4.0;
/// How much of a tool call's arguments a collapsed summary shows.
const COLLAPSED_ARGS_LEN: usize = 60;
/// How far a user bubble is indented, so the two sides of the
/// conversation do not share a left edge.
const USER_BUBBLE_INDENT: f32 = 48.0;

/// Colour for a notice of the given severity. These are the same colours
/// the flat-line shape used: red for an error, orange for an interrupt,
/// cyan for a session reset, grey for anything quieter.
fn severity_color(severity: Severity) -> Color32 {
    match severity {
        Severity::Error => Color32::from_rgb(255, 80, 80),
        Severity::Warning => Color32::from_rgb(255, 165, 0),
        Severity::Info => Color32::from_rgb(0, 255, 255),
        Severity::Debug => Color32::from_rgb(128, 128, 128),
    }
}

/// The label naming who or what produced a block.
fn role_label(kind: &BlockKind) -> &'static str {
    match kind {
        BlockKind::User { .. } => "You",
        BlockKind::Assistant { .. } => "Assistant",
        BlockKind::ToolCall { .. } => "Tool",
        BlockKind::Notice { .. } => "Notice",
        BlockKind::Subagent { .. } => "Subagent",
        BlockKind::Image { .. } => "Image",
    }
}

/// Vertical space to leave above a block. A user message opens a turn, so
/// the wider gap in front of one is what makes a turn boundary visible.
fn gap_before(index: usize, kind: &BlockKind) -> f32 {
    match kind {
        BlockKind::User { .. } if index > 0 => TURN_GAP,
        _ => BLOCK_GAP,
    }
}

/// The frame a message bubble draws itself in.
fn bubble_frame(fill: Color32) -> egui::Frame {
    egui::Frame::new()
        .fill(fill)
        .corner_radius(6)
        .inner_margin(egui::Margin::symmetric(8, 6))
}

/// The user's message as an indented bubble with a role label.
fn render_user_bubble(ui: &mut egui::Ui, label: &str, text: &str) {
    ui.horizontal(|ui| {
        ui.add_space(USER_BUBBLE_INDENT);
        bubble_frame(USER_BUBBLE_FILL).show(ui, |ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new(label).color(USER_ROLE_COLOR).small());
                ui.label(RichText::new(text));
            });
        });
    });
}

/// An `Image` block: a thumbnail capped to `IMAGE_THUMBNAIL_MAX` on its
/// longer side, clickable to open the same image full size in its own
/// window. The open/closed state of that window lives in egui's own
/// per-widget temp storage, keyed off `block_id`, rather than on
/// `DeepSeekGui` or on the block itself: no other block needs GUI-only
/// state, and adding a field for just this one kind would leak a rendering
/// concern back into the plain-data transcript model.
fn render_image_block(ui: &mut egui::Ui, block_id: BlockId, image: &ImageAttachment) {
    let Some(bytes) = decode_image_bytes(image) else {
        ui.colored_label(
            IMAGE_LABEL_COLOR,
            format!("[image: {} - could not decode]", image.media_type),
        );
        return;
    };
    let uri = format!("bytes://image-block-{block_id:?}");
    let open_id = egui::Id::new(("image-block-open", block_id));
    let mut open = ui.data(|data| data.get_temp::<bool>(open_id).unwrap_or(false));

    let thumbnail = egui::Image::from_bytes(uri.clone(), bytes.clone())
        .max_size(egui::vec2(IMAGE_THUMBNAIL_MAX, IMAGE_THUMBNAIL_MAX))
        .sense(egui::Sense::click());
    let response = ui.add(thumbnail).on_hover_text("Click to view full size");
    if response.clicked() {
        open = !open;
    }

    if open {
        egui::Window::new(format!("Image ({})", image.media_type))
            .id(egui::Id::new(("image-block-window", block_id)))
            .open(&mut open)
            .show(ui.ctx(), |ui| {
                ui.add(egui::Image::from_bytes(uri, bytes));
            });
    }
    ui.data_mut(|data| data.insert_temp(open_id, open));
}

/// A tool call as one clickable summary line that opens to show the full
/// arguments and the output. The click is reported through `toggled`, not
/// applied here, since the block is only held by shared reference. `path`
/// is the full path to this block (see `DeepSeekGui::render_block`),
/// recorded on a click rather than the bare id.
fn render_tool_call(
    ui: &mut egui::Ui,
    block: &Block,
    path: &[BlockId],
    toggled: &mut Vec<Vec<BlockId>>,
) {
    let BlockKind::ToolCall {
        tool,
        args,
        output,
        is_error,
    } = &block.kind
    else {
        return;
    };
    let summary = tool_summary(block.collapsed, tool, args, *is_error);
    let color = tool_color(*is_error);
    let line = egui::Label::new(RichText::new(summary).color(color))
        .sense(egui::Sense::click())
        .wrap_mode(egui::TextWrapMode::Truncate);
    if ui.add(line).clicked() {
        toggled.push(path.to_vec());
    }
    if block.collapsed {
        return;
    }
    ui.indent(block.id, |ui| {
        ui.label(RichText::new(args).color(color).monospace());
        if let Some(output) = output {
            ui.label(
                RichText::new(output)
                    .color(tool_output_color(*is_error))
                    .monospace(),
            );
        }
    });
}

/// The one-line summary of a tool call. It names the tool and shows
/// enough of the arguments to recognise the call. An errored call says so
/// in the text, so a reader does not have to open it to see the failure.
fn tool_summary(collapsed: bool, tool: &str, args: &str, is_error: bool) -> String {
    let marker = if collapsed { '\u{25b6}' } else { '\u{25bc}' };
    let error = if is_error { " [error]" } else { "" };
    let args = truncate_args(args, COLLAPSED_ARGS_LEN);
    format!("{marker} {tool}{error}  {args}")
}

/// Flatten arguments onto one line and cut them to `max` characters, so a
/// collapsed tool call stays one line however long its arguments are.
fn truncate_args(args: &str, max: usize) -> String {
    let flat = args.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let cut: String = flat.chars().take(max).collect();
    format!("{cut}...")
}

/// Colour of a tool call's own lines. An error is red in both the
/// collapsed and the expanded state.
fn tool_color(is_error: bool) -> Color32 {
    if is_error {
        TOOL_ERROR_COLOR
    } else {
        TOOL_COLOR
    }
}

/// Colour of a tool call's output body.
fn tool_output_color(is_error: bool) -> Color32 {
    if is_error {
        TOOL_ERROR_COLOR
    } else {
        TOOL_OUTPUT_COLOR
    }
}

/// Draw every block as one plain coloured line, for the raw-output
/// toggle. This is the only flattening left: the rendered path draws each
/// block on its own terms instead.
fn render_raw_blocks(ui: &mut egui::Ui, blocks: &[Block]) {
    for block in blocks {
        ui.label(RichText::new(raw_block_text(block)).color(block_color(&block.kind)));
    }
}

/// One block as raw text for the raw-output toggle. Every field the
/// rendered path shows for a block also has to be reachable here: a role
/// label for a `User` or `Assistant` block, a marked-apart reasoning span,
/// and a tool call's arguments and output.
fn raw_block_text(block: &Block) -> String {
    match &block.kind {
        BlockKind::User { text } => format!("{}: {text}", role_label(&block.kind)),
        BlockKind::Assistant { spans } => {
            let body = spans
                .iter()
                .map(raw_span_text)
                .collect::<Vec<_>>()
                .join("\n");
            format!("{}:\n{body}", role_label(&block.kind))
        }
        BlockKind::ToolCall {
            tool, args, output, ..
        } => {
            let body = output.as_deref().unwrap_or("(running)");
            format!("\u{2699} {tool} {args}\n  \u{2192} {body}")
        }
        BlockKind::Notice { text, .. } => text.clone(),
        BlockKind::Image { image } => format!(
            "[image: {}, {} bytes base64]",
            image.media_type,
            image.data.len()
        ),
        BlockKind::Subagent {
            backend,
            model,
            depth,
            state,
            elapsed_ms,
            started_at,
            transcript,
            session_turns,
            session_turn_cap,
            send_message_calls,
            send_message_call_cap,
            ..
        } => {
            let elapsed = subagent_elapsed_ms(*started_at, *elapsed_ms);
            let header = subagent_header_summary(
                backend,
                model,
                *depth,
                *state,
                elapsed,
                *session_turns,
                *session_turn_cap,
                *send_message_calls,
                *send_message_call_cap,
            );
            let body = transcript
                .blocks()
                .iter()
                .map(raw_block_text)
                .collect::<Vec<_>>()
                .join("\n");
            if body.is_empty() {
                header
            } else {
                let indented = body
                    .lines()
                    .map(|line| format!("  {line}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("{header}\n{indented}")
            }
        }
    }
}

/// One assistant span as raw text, with reasoning marked so it does not
/// read as part of the reply.
fn raw_span_text(span: &Span) -> String {
    match span {
        Span::Text(text) => text.clone(),
        Span::Reasoning(text) => format!("[reasoning] {text}"),
    }
}

/// Colour for a whole block on the raw path, chosen from its kind alone.
fn block_color(kind: &BlockKind) -> Color32 {
    match kind {
        BlockKind::User { .. } => USER_ROLE_COLOR,
        BlockKind::Assistant { .. } => Color32::WHITE,
        BlockKind::ToolCall { is_error, .. } => tool_color(*is_error),
        BlockKind::Notice { severity, .. } => severity_color(*severity),
        BlockKind::Subagent { state, .. } => subagent_state_color(*state),
        BlockKind::Image { .. } => IMAGE_LABEL_COLOR,
    }
}

/// The elapsed time to show for a `Subagent` block: live, computed from
/// `started_at`, while the dispatch is still running, or the stored value
/// once it has reached a terminal state and `started_at` has been taken.
fn subagent_elapsed_ms(started_at: Option<Instant>, stored_elapsed_ms: u64) -> u64 {
    match started_at {
        Some(start) => start.elapsed().as_millis() as u64,
        None => stored_elapsed_ms,
    }
}

/// The short word shown in a `Subagent` block's header for each state.
fn subagent_state_label(state: SubagentState) -> &'static str {
    match state {
        SubagentState::Running => "RUNNING",
        SubagentState::Done => "DONE",
        SubagentState::Failed => "FAILED",
        SubagentState::Interrupted => "INTERRUPTED",
    }
}

/// Colour of a `Subagent` block's state badge and, on the raw path, its
/// whole line.
fn subagent_state_color(state: SubagentState) -> Color32 {
    match state {
        SubagentState::Running => Color32::from_rgb(150, 150, 255),
        SubagentState::Done => Color32::from_rgb(100, 200, 100),
        SubagentState::Failed => TOOL_ERROR_COLOR,
        SubagentState::Interrupted => Color32::from_rgb(255, 165, 0),
    }
}

/// Format a millisecond count as seconds with one decimal place, matching
/// how the rest of this file reports durations to the user.
fn format_elapsed_ms(elapsed_ms: u64) -> String {
    format!("{:.1}s", elapsed_ms as f64 / 1000.0)
}

/// The one-line header summary for a `Subagent` block: state badge,
/// backend and model, depth, elapsed time, and both runaway-cost counts
/// from the roadmap's Phase 3 section against their caps (the session's
/// own turn count, and its owner's total `SendMessage` calls this turn).
/// Shared by the collapsing header and the raw-output path, so both name
/// the same facts about a dispatch. Shown while the session is live and
/// under both caps, not only once one of them trips: a count that only
/// appears at the limit is not a warning, it is a surprise.
#[allow(clippy::too_many_arguments)]
fn subagent_header_summary(
    backend: &str,
    model: &str,
    depth: u32,
    state: SubagentState,
    elapsed_ms: u64,
    session_turns: u32,
    session_turn_cap: u32,
    send_message_calls: u32,
    send_message_call_cap: u32,
) -> String {
    format!(
        "[{}] {backend}/{model} (depth {depth}) - {} - turns {session_turns}/{session_turn_cap} \
        - sends {send_message_calls}/{send_message_call_cap}",
        subagent_state_label(state),
        format_elapsed_ms(elapsed_ms)
    )
}

#[cfg(test)]
mod tests {
    use super::autopilot_tab::{
        AutopilotProgress, apply_autopilot_iterations, apply_autopilot_task,
    };
    use super::settings_panel::{
        apply_backend_model, apply_context_budget, apply_default_backend, apply_effort,
        apply_show_raw_output, apply_working_dir,
    };
    use super::voice_ui::{
        apply_stt_enabled, apply_trigger_mode, apply_tts_enabled, apply_tts_speed, apply_tts_voice,
        apply_voice_enabled, apply_wake_phrase, voice_mode_flag_for_tts,
    };
    use super::*;
    use crate::agent::agent_loop::{RouteHop, SubagentId, SubagentMeta};
    use crate::api::types::{Content, Message, Role};
    use crate::config::settings::{ApiProvider, BackendConfig, TriggerMode, VoiceConfig};
    use std::collections::HashMap;

    fn make_gui() -> DeepSeekGui {
        make_gui_with_settings(&Settings::default())
    }

    /// Create a uniquely named directory under the system temp dir, so a
    /// test that saves never touches the repository's real settings.json.
    fn unique_temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("dsc-gui-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Build a GUI from a specific settings value, so a test can check how
    /// the panel controls get seeded.
    fn make_gui_with_settings(settings: &Settings) -> DeepSeekGui {
        make_gui_in(settings, unique_temp_dir("seed"))
    }

    /// Build a GUI over a specific project root, for tests that save.
    fn make_gui_in(settings: &Settings, project_root: PathBuf) -> DeepSeekGui {
        let (_tx_events, rx_events) = mpsc::unbounded_channel();
        let (tx_input, _rx_input) = mpsc::unbounded_channel();
        // Mirrors main.rs: the effort flag is seeded from settings before
        // the GUI is constructed, so `new` can read the starting level back
        // off the flag the same way it does for `context_budget_flag`.
        let effort_flag = Arc::new(AtomicU8::new(0));
        settings.effort().store(&effort_flag);
        DeepSeekGui::new(
            rx_events,
            tx_input,
            Arc::new(AtomicBool::new(false)),
            effort_flag,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicUsize::new(100_000)),
            Arc::new(Mutex::new("deepseek-v4-flash".into())),
            Arc::new(Mutex::new(project_root.clone())),
            settings.clone(),
            project_root,
        )
    }

    /// A settings value with every panel control set away from its default.
    fn settings_with_voice(tts_voice: &str) -> Settings {
        Settings {
            show_raw_output: Some(true),
            voice: Some(VoiceConfig {
                enabled: true,
                stt_enabled: true,
                tts_enabled: true,
                stt_model_path: None,
                tts_model_path: None,
                tts_voices_path: None,
                trigger_mode: TriggerMode::WakeWord,
                wake_phrase: Some("hey computer".to_string()),
                tts_voice: Some(tts_voice.to_string()),
                tts_speed: Some(1.4),
            }),
            ..Settings::default()
        }
    }

    /// A settings value with two backends, "alpha" and "beta", and the
    /// given `default_backend`.
    fn settings_with_backends(default_backend: Option<&str>) -> Settings {
        let mut backends = HashMap::new();
        backends.insert(
            "alpha".to_string(),
            BackendConfig::Api {
                provider: ApiProvider::DeepSeek,
                model: "alpha-model".to_string(),
                base_url: None,
                api_key: None,
                models: None,
            },
        );
        backends.insert(
            "beta".to_string(),
            BackendConfig::ClaudeCli {
                model: "beta-model".to_string(),
                permission_mode: None,
                env: None,
                models: None,
            },
        );
        Settings {
            backends: Some(backends),
            default_backend: default_backend.map(|s| s.to_string()),
            ..Settings::default()
        }
    }

    #[test]
    fn new_seeds_the_backend_picker_from_default_backend() {
        let gui = make_gui_with_settings(&settings_with_backends(Some("beta")));
        assert_eq!(gui.backend_options, vec!["alpha", "beta"]);
        assert_eq!(gui.backend_options[gui.selected_backend_idx], "beta");
    }

    #[test]
    fn new_falls_back_to_the_first_sorted_backend_when_default_is_absent() {
        let gui = make_gui_with_settings(&settings_with_backends(None));
        assert_eq!(gui.selected_backend_idx, 0);
        assert_eq!(gui.backend_options[0], "alpha");
    }

    #[test]
    fn new_falls_back_to_the_first_sorted_backend_when_default_is_unknown() {
        let gui = make_gui_with_settings(&settings_with_backends(Some("nonexistent")));
        assert_eq!(gui.selected_backend_idx, 0);
        assert_eq!(gui.backend_options[0], "alpha");
    }

    #[test]
    fn apply_backend_model_writes_onto_named_backend_and_round_trips() {
        let dir = unique_temp_dir("apply-backend-model-roundtrip");
        let mut settings = settings_with_backends(Some("alpha"));
        apply_backend_model(&mut settings, "alpha", "new-model");
        settings.save(&dir).unwrap();

        let loaded = Settings::load(&dir).unwrap();
        match loaded.resolve_backend("alpha").unwrap() {
            BackendConfig::Api { model, .. } => assert_eq!(model, "new-model"),
            BackendConfig::ClaudeCli { .. } => panic!("expected Api variant"),
        }

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn apply_backend_model_on_unknown_backend_is_a_no_op() {
        let mut settings = settings_with_backends(Some("alpha"));
        apply_backend_model(&mut settings, "nonexistent", "new-model");
        match settings.resolve_backend("alpha").unwrap() {
            BackendConfig::Api { model, .. } => assert_eq!(model, "alpha-model"),
            BackendConfig::ClaudeCli { .. } => panic!("expected Api variant"),
        }
    }

    #[test]
    fn apply_backend_model_works_for_the_claude_cli_variant() {
        let mut settings = settings_with_backends(Some("beta"));
        apply_backend_model(&mut settings, "beta", "sonnet");
        match settings.resolve_backend("beta").unwrap() {
            BackendConfig::ClaudeCli { model, .. } => assert_eq!(model, "sonnet"),
            BackendConfig::Api { .. } => panic!("expected ClaudeCli variant"),
        }
    }

    #[test]
    fn switch_backend_updates_selected_model_to_the_new_backends_declared_model() {
        let mut gui = make_gui_with_settings(&settings_with_backends(Some("alpha")));
        gui.switch_backend("beta".to_string());
        assert_eq!(gui.model, "beta-model");
        gui.switch_backend("alpha".to_string());
        assert_eq!(gui.model, "alpha-model");
    }

    #[test]
    fn switch_backend_seeds_model_options_with_the_declared_model() {
        let mut gui = make_gui_with_settings(&settings_with_backends(Some("alpha")));
        gui.switch_backend("beta".to_string());
        assert!(gui.model_options.contains(&"beta-model".to_string()));
    }

    #[tokio::test]
    async fn model_options_always_contain_the_backends_declared_model_after_a_fetch() {
        let mut gui = make_gui_with_settings(&settings_with_backends(Some("alpha")));
        let (backend_name, models) = gui.model_list_rx.recv().await.unwrap();
        assert_eq!(backend_name, "alpha");
        gui.apply_fetched_model_list(&backend_name, models);
        assert!(gui.model_options.contains(&gui.model));
    }

    /// The spans of the only block in the transcript, which must be an
    /// `Assistant` block. Panics otherwise, so a test that expects
    /// assistant output fails loudly on any other block kind.
    fn only_assistant_spans(gui: &DeepSeekGui) -> Vec<Span> {
        let blocks = gui.transcript.blocks();
        assert_eq!(blocks.len(), 1, "expected exactly one block");
        let BlockKind::Assistant { spans } = &blocks[0].kind else {
            panic!("expected an Assistant block, got {:?}", blocks[0].kind);
        };
        spans.clone()
    }

    #[test]
    fn reasoning_event_adds_payload_line() {
        let mut gui = make_gui();
        gui.handle_stream_event(StreamEvent::Reasoning {
            turn: 1,
            text: "Let me think about this...".into(),
        });
        assert_eq!(
            only_assistant_spans(&gui),
            vec![Span::Reasoning("Let me think about this...".into())]
        );
    }

    #[test]
    fn reasoning_event_appends_to_last_reasoning_line() {
        let mut gui = make_gui();
        gui.handle_stream_event(StreamEvent::Reasoning {
            turn: 1,
            text: "First".into(),
        });
        gui.handle_stream_event(StreamEvent::Reasoning {
            turn: 1,
            text: "Second".into(),
        });
        assert_eq!(
            only_assistant_spans(&gui),
            vec![Span::Reasoning("FirstSecond".into())]
        );
    }

    fn tool_block(args: &str, output: Option<&str>, is_error: bool) -> Block {
        let mut transcript = Transcript::new();
        let id = transcript.push(BlockKind::ToolCall {
            tool: "Bash".into(),
            args: args.into(),
            output: output.map(str::to_string),
            is_error,
        });
        transcript.find(id).unwrap().clone()
    }

    #[test]
    fn role_label_names_each_kind() {
        assert_eq!(role_label(&BlockKind::User { text: "hi".into() }), "You");
        assert_eq!(
            role_label(&BlockKind::Assistant { spans: Vec::new() }),
            "Assistant"
        );
        assert_eq!(role_label(&tool_block("ls", None, false).kind), "Tool");
        assert_eq!(
            role_label(&BlockKind::Notice {
                text: "reset".into(),
                severity: Severity::Info,
            }),
            "Notice"
        );
        assert_eq!(
            role_label(&BlockKind::Image {
                image: test_image_attachment(),
            }),
            "Image"
        );
    }

    #[test]
    fn severity_color_is_distinct_per_severity() {
        let colors = [
            severity_color(Severity::Error),
            severity_color(Severity::Warning),
            severity_color(Severity::Info),
            severity_color(Severity::Debug),
        ];
        for (index, color) in colors.iter().enumerate() {
            for other in &colors[index + 1..] {
                assert_ne!(color, other, "each severity needs its own colour");
            }
        }
    }

    #[test]
    fn truncate_args_leaves_short_arguments_alone() {
        assert_eq!(truncate_args("ls -la", 60), "ls -la");
    }

    #[test]
    fn truncate_args_flattens_newlines_onto_one_line() {
        assert_eq!(truncate_args("echo one\necho two", 60), "echo one echo two");
    }

    #[test]
    fn truncate_args_cuts_long_arguments_and_marks_the_cut() {
        let truncated = truncate_args(&"x".repeat(100), 10);
        assert_eq!(truncated, format!("{}...", "x".repeat(10)));
    }

    #[test]
    fn truncate_args_counts_characters_not_bytes() {
        // A cut by byte index would panic here, since each character is
        // three bytes wide.
        let truncated = truncate_args(&"\u{4f60}".repeat(10), 4);
        assert_eq!(truncated, format!("{}...", "\u{4f60}".repeat(4)));
    }

    #[test]
    fn a_collapsed_tool_summary_names_the_tool_and_shows_arguments() {
        let summary = tool_summary(true, "Bash", "ls -la", false);
        assert!(summary.contains("Bash"), "summary must name the tool");
        assert!(summary.contains("ls -la"), "summary must show arguments");
    }

    #[test]
    fn an_errored_tool_summary_says_so_in_both_states() {
        for collapsed in [true, false] {
            let summary = tool_summary(collapsed, "Bash", "boom", true);
            assert!(
                summary.contains("[error]"),
                "an error must be visible without expanding the call"
            );
        }
        assert!(!tool_summary(true, "Bash", "ok", false).contains("[error]"));
    }

    #[test]
    fn an_errored_tool_call_is_red_in_both_states() {
        assert_eq!(tool_color(true), TOOL_ERROR_COLOR);
        assert_eq!(tool_output_color(true), TOOL_ERROR_COLOR);
        assert_ne!(tool_color(false), TOOL_ERROR_COLOR);
        assert_ne!(tool_output_color(false), TOOL_ERROR_COLOR);
    }

    #[test]
    fn a_user_block_after_the_first_opens_a_turn_with_a_wider_gap() {
        let user = BlockKind::User {
            text: "hello".into(),
        };
        assert_eq!(gap_before(1, &user), TURN_GAP);
        assert_eq!(gap_before(3, &user), TURN_GAP);
    }

    #[test]
    fn the_first_block_and_non_user_blocks_get_the_plain_gap() {
        let user = BlockKind::User {
            text: "hello".into(),
        };
        assert_eq!(gap_before(0, &user), BLOCK_GAP);
        assert_eq!(
            gap_before(2, &BlockKind::Assistant { spans: Vec::new() }),
            BLOCK_GAP
        );
        assert!(TURN_GAP > BLOCK_GAP, "a turn boundary must read as wider");
    }

    #[test]
    fn raw_text_marks_reasoning_apart_from_reply_text() {
        let mut transcript = Transcript::new();
        let id = transcript.push(BlockKind::Assistant {
            spans: vec![Span::Reasoning("thought".into()), Span::Text("said".into())],
        });
        let block = transcript.find(id).unwrap();
        assert_eq!(
            raw_block_text(block),
            "Assistant:\n[reasoning] thought\nsaid"
        );
    }

    #[test]
    fn raw_text_for_a_running_tool_call_says_it_is_running() {
        let block = tool_block("ls", None, false);
        assert!(raw_block_text(&block).contains("(running)"));
        let done = tool_block("ls", Some("file1"), false);
        assert!(raw_block_text(&done).contains("file1"));
    }

    #[test]
    fn raw_text_for_a_tool_call_carries_its_arguments_and_output() {
        let block = tool_block("ls -la", Some("file1\nfile2"), false);
        let text = raw_block_text(&block);
        assert!(
            text.contains("ls -la"),
            "arguments must be reachable: {text}"
        );
        assert!(
            text.contains("file1\nfile2"),
            "output must be reachable: {text}"
        );
    }

    #[test]
    fn raw_text_for_a_user_block_carries_the_you_role_label_and_exact_text() {
        let mut transcript = Transcript::new();
        let id = transcript.push(BlockKind::User {
            text: "hello there".into(),
        });
        let block = transcript.find(id).unwrap();
        assert_eq!(raw_block_text(block), "You: hello there");
    }

    #[test]
    fn raw_text_for_a_notice_block_carries_its_text() {
        let mut transcript = Transcript::new();
        let id = transcript.push(BlockKind::Notice {
            text: "Session reset".into(),
            severity: Severity::Info,
        });
        let block = transcript.find(id).unwrap();
        assert_eq!(raw_block_text(block), "Session reset");
    }

    #[test]
    fn raw_text_for_a_subagent_block_carries_its_header_and_nested_content() {
        let mut transcript = Transcript::new();
        let subagent_id = SubagentId::next();
        transcript.apply_routed_event(RoutedEvent {
            route: vec![RouteHop {
                id: subagent_id,
                meta: SubagentMeta {
                    backend: "ollama".into(),
                    model: "test-model".into(),
                    depth: 1,
                },
                session_turns: 1,
                session_turn_cap: 20,
                send_message_calls: 0,
                send_message_call_cap: 10,
            }],
            event: StreamEvent::Text {
                turn: 1,
                text: "hi from a subagent".into(),
            },
        });
        let block = &transcript.blocks()[0];
        let text = raw_block_text(block);
        assert!(text.contains("ollama"), "must name the backend: {text}");
        assert!(text.contains("test-model"), "must name the model: {text}");
        assert!(text.contains("RUNNING"), "must show the state: {text}");
        assert!(
            text.contains("hi from a subagent"),
            "must carry the nested content: {text}"
        );
        assert!(
            text.contains("turns 1/20"),
            "must show the turn count and cap: {text}"
        );
        assert!(
            text.contains("sends 0/10"),
            "must show the call count and cap: {text}"
        );
    }

    #[test]
    fn raw_span_text_marks_reasoning_and_leaves_reply_text_plain() {
        assert_eq!(raw_span_text(&Span::Text("hello".into())), "hello");
        assert_eq!(
            raw_span_text(&Span::Reasoning("thinking".into())),
            "[reasoning] thinking"
        );
    }

    #[test]
    fn block_color_follows_kind_and_severity() {
        assert_eq!(
            block_color(&BlockKind::Assistant { spans: Vec::new() }),
            Color32::WHITE
        );
        assert_eq!(
            block_color(&tool_block("ls", None, true).kind),
            TOOL_ERROR_COLOR
        );
        assert_eq!(
            block_color(&BlockKind::Notice {
                text: "boom".into(),
                severity: Severity::Error,
            }),
            severity_color(Severity::Error)
        );
        assert_eq!(
            block_color(&BlockKind::Image {
                image: test_image_attachment(),
            }),
            IMAGE_LABEL_COLOR
        );
    }

    /// A tiny, real base64 payload for image-block tests: the 1x1
    /// transparent PNG egui's own examples use. Its bytes are not
    /// inspected, so any well-formed base64 string would do, but a real
    /// PNG keeps the fixture honest about what this block actually holds.
    fn test_image_attachment() -> ImageAttachment {
        ImageAttachment {
            data: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=".into(),
            media_type: "image/png".into(),
        }
    }

    #[test]
    fn decode_image_bytes_accepts_well_formed_base64() {
        let image = test_image_attachment();
        let bytes = decode_image_bytes(&image).expect("valid base64 must decode");
        // PNG's fixed 8-byte magic number.
        assert_eq!(
            &bytes[..8],
            &[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n']
        );
    }

    #[test]
    fn decode_image_bytes_rejects_malformed_base64() {
        let image = ImageAttachment {
            data: "not valid base64 !!!".into(),
            media_type: "image/png".into(),
        };
        assert!(decode_image_bytes(&image).is_none());
    }

    #[test]
    fn raw_block_text_names_the_image_media_type() {
        let mut transcript = Transcript::new();
        let id = transcript.push(BlockKind::Image {
            image: test_image_attachment(),
        });
        let block = transcript.find(id).unwrap();
        let text = raw_block_text(block);
        assert!(
            text.contains("image/png"),
            "must name the media type: {text}"
        );
    }

    #[test]
    fn submit_current_input_attaches_the_pending_image_and_clears_the_strip() {
        let mut gui = make_gui();
        gui.attachment
            .set(test_image_attachment(), &mut gui.transcript);
        gui.input_buffer = "look at this".into();

        gui.submit_current_input();

        assert!(gui.attachment.is_empty(), "the strip must clear on send");
        let blocks = gui.transcript.blocks();
        assert_eq!(blocks.len(), 2, "a User block and an Image block");
        assert!(matches!(blocks[0].kind, BlockKind::User { .. }));
        assert!(matches!(blocks[1].kind, BlockKind::Image { .. }));
    }

    /// The image belongs on `AgentCommand::UserTurn` itself, not on a side
    /// channel delivered separately. This is the fix for P6S05: a shared
    /// `Arc<Mutex<Option<ImageAttachment>>>` used to carry the image next
    /// to the command, with no guarantee the two paired up. Proves text
    /// and image now arrive on the very same command.
    #[test]
    fn submit_current_input_sends_text_and_image_on_the_same_command() {
        let mut gui = make_gui();
        let (tx_input, mut rx_input) = mpsc::unbounded_channel();
        gui.tx_input = tx_input;
        gui.attachment
            .set(test_image_attachment(), &mut gui.transcript);
        gui.input_buffer = "look at this".into();

        gui.submit_current_input();

        match rx_input.try_recv().unwrap() {
            AgentCommand::UserTurn { text, image } => {
                assert_eq!(text, "look at this");
                assert_eq!(image, Some(test_image_attachment()));
            }
            other => panic!("expected UserTurn, got {other:?}"),
        }
    }

    /// Two turns sent back to back must each carry their own attachment,
    /// never the other's. This is exactly the mis-pairing a shared side
    /// channel allowed: a second send could overwrite the first turn's
    /// image before the agent task read it out.
    #[test]
    fn two_turns_in_a_row_each_carry_their_own_attachment() {
        let mut gui = make_gui();
        let (tx_input, mut rx_input) = mpsc::unbounded_channel();
        gui.tx_input = tx_input;

        gui.attachment
            .set(test_image_attachment(), &mut gui.transcript);
        gui.input_buffer = "first turn".into();
        gui.submit_current_input();

        gui.input_buffer = "second turn".into();
        gui.submit_current_input();

        match rx_input.try_recv().unwrap() {
            AgentCommand::UserTurn { text, image } => {
                assert_eq!(text, "first turn");
                assert_eq!(image, Some(test_image_attachment()));
            }
            other => panic!("expected UserTurn, got {other:?}"),
        }
        match rx_input.try_recv().unwrap() {
            AgentCommand::UserTurn { text, image } => {
                assert_eq!(text, "second turn");
                assert_eq!(
                    image, None,
                    "the second turn must not inherit the first's image"
                );
            }
            other => panic!("expected UserTurn, got {other:?}"),
        }
    }

    #[test]
    fn submit_current_input_sends_an_image_with_no_text() {
        let mut gui = make_gui();
        gui.attachment
            .set(test_image_attachment(), &mut gui.transcript);
        assert!(gui.input_buffer.is_empty());

        gui.submit_current_input();

        assert_eq!(
            gui.session_status, "Running...",
            "an image-only turn must still send"
        );
        assert!(gui.attachment.is_empty());
    }

    #[test]
    fn subagent_header_summary_names_backend_model_depth_and_elapsed() {
        let header = subagent_header_summary(
            "ollama",
            "test-model",
            2,
            SubagentState::Running,
            1500,
            1,
            20,
            0,
            10,
        );
        assert!(header.contains("ollama"), "must name the backend: {header}");
        assert!(
            header.contains("test-model"),
            "must name the model: {header}"
        );
        assert!(header.contains('2'), "must name the depth: {header}");
        assert!(header.contains("1.5s"), "must show elapsed time: {header}");
    }

    /// The header must name both runaway-cost counts against their caps,
    /// and it must do so while the session is still comfortably under
    /// both: the roadmap calls for the counts to be visible the whole
    /// time a session is open, not only once a cap trips.
    #[test]
    fn subagent_header_summary_names_both_counts_against_their_caps() {
        let header = subagent_header_summary(
            "ollama",
            "test-model",
            1,
            SubagentState::Running,
            0,
            3,
            20,
            2,
            10,
        );
        assert!(
            header.contains("3/20"),
            "must show turns against its cap: {header}"
        );
        assert!(
            header.contains("2/10"),
            "must show sends against its cap: {header}"
        );
    }

    #[test]
    fn subagent_header_summary_carries_a_distinct_badge_for_every_state() {
        let states = [
            SubagentState::Running,
            SubagentState::Done,
            SubagentState::Failed,
            SubagentState::Interrupted,
        ];
        let headers: Vec<String> = states
            .iter()
            .map(|state| subagent_header_summary("ollama", "m", 1, *state, 0, 1, 20, 0, 10))
            .collect();
        for (index, header) in headers.iter().enumerate() {
            for other in &headers[index + 1..] {
                assert_ne!(header, other, "each state needs its own header text");
            }
        }
        assert!(headers[0].contains("RUNNING"));
        assert!(headers[1].contains("DONE"));
        assert!(headers[2].contains("FAILED"));
        assert!(headers[3].contains("INTERRUPTED"));
    }

    #[test]
    fn subagent_state_color_is_distinct_per_state() {
        let colors = [
            subagent_state_color(SubagentState::Running),
            subagent_state_color(SubagentState::Done),
            subagent_state_color(SubagentState::Failed),
            subagent_state_color(SubagentState::Interrupted),
        ];
        for (index, color) in colors.iter().enumerate() {
            for other in &colors[index + 1..] {
                assert_ne!(color, other, "each state needs its own colour");
            }
        }
    }

    #[test]
    fn subagent_elapsed_ms_uses_the_stored_value_once_terminal() {
        assert_eq!(subagent_elapsed_ms(None, 4200), 4200);
    }

    #[test]
    fn subagent_elapsed_ms_computes_a_live_value_while_running() {
        let start = Instant::now() - Duration::from_millis(50);
        let live = subagent_elapsed_ms(Some(start), 0);
        assert!(
            live >= 50,
            "a running block's elapsed time must grow from `started_at`, got {live}"
        );
    }

    #[test]
    fn format_elapsed_ms_shows_one_decimal_of_seconds() {
        assert_eq!(format_elapsed_ms(1500), "1.5s");
        assert_eq!(format_elapsed_ms(0), "0.0s");
    }

    #[test]
    fn text_event_creates_white_payload_lines() {
        let mut gui = make_gui();
        gui.handle_stream_event(StreamEvent::Text {
            turn: 1,
            text: "Hello world".into(),
        });
        // The colour this used to assert on now lives in the block kind:
        // a `Text` span inside an `Assistant` block is what the renderer
        // draws white and passes to the markdown viewer.
        assert_eq!(
            only_assistant_spans(&gui),
            vec![Span::Text("Hello world".into())]
        );
    }

    #[test]
    fn user_input_is_a_user_block_not_an_assistant_one() {
        let mut gui = make_gui();
        gui.input_buffer = "pick a number between 1 and 100".into();
        gui.submit_current_input();
        let blocks = gui.transcript.blocks();
        assert_eq!(blocks.len(), 1, "exactly one block must be appended");
        assert_eq!(
            blocks[0].kind,
            BlockKind::User {
                text: "pick a number between 1 and 100".into(),
            },
            "user input must be a User block, never assistant output"
        );

        // A second submission must not merge into the first: unlike an
        // Assistant block's spans, a User block never coalesces with an
        // earlier one.
        gui.input_buffer = "actually pick a letter instead".into();
        gui.submit_current_input();
        let blocks = gui.transcript.blocks();
        assert_eq!(
            blocks.len(),
            2,
            "a second submission must append a new block, not merge"
        );
        assert_eq!(
            blocks[0].kind,
            BlockKind::User {
                text: "pick a number between 1 and 100".into(),
            },
            "the first block's text must stay untouched by the second submission"
        );
        assert_eq!(
            blocks[1].kind,
            BlockKind::User {
                text: "actually pick a letter instead".into(),
            }
        );
        assert_ne!(blocks[0].id, blocks[1].id);
    }

    #[test]
    fn model_output_is_an_assistant_text_span_for_markdown_rendering() {
        let mut gui = make_gui();
        gui.handle_stream_event(StreamEvent::Text {
            turn: 1,
            text: "**bold** ".into(),
        });
        gui.handle_stream_event(StreamEvent::Text {
            turn: 1,
            text: "and *italic*".into(),
        });
        // Two deltas of the same kind must coalesce into exactly one Text
        // span with the concatenated content, not one span per delta, and
        // must land in exactly one Assistant block (checked inside
        // only_assistant_spans).
        assert_eq!(
            only_assistant_spans(&gui),
            vec![Span::Text("**bold** and *italic*".into())],
            "model output must coalesce into a single Text span, the kind the renderer treats as markdown"
        );
    }

    /// Simulates a full thinking-enabled interaction: user input ->
    /// reasoning -> a tool call -> more text -> turn end. Proves the block
    /// kinds now carry what the line colours used to: the user's message
    /// is a `User` block, reasoning and reply text stay separate spans of
    /// one `Assistant` block each, and a tool call in between splits the
    /// reply into two `Assistant` blocks around one filled `ToolCall`
    /// block, exactly as the scripted events imply.
    #[test]
    fn full_thinking_interaction_produces_correct_block_kinds() {
        let mut gui = make_gui();

        // 1. User presses Enter.
        gui.input_buffer = "pick a number between 1 and 100 but don't tell me".into();
        gui.submit_current_input();

        // 2. Reasoning chunk arrives from agent (thinking enabled)
        gui.handle_stream_event(StreamEvent::Reasoning {
            turn: 1,
            text: "The user wants me to pick a secret number.".into(),
        });

        // 3. More reasoning (coalesces into the same span)
        gui.handle_stream_event(StreamEvent::Reasoning {
            turn: 1,
            text: " I'll pick 42.".into(),
        });

        // 4. The agent calls a tool before replying.
        gui.handle_stream_event(StreamEvent::ToolCallStart {
            turn: 1,
            tool: "Bash".into(),
            args: "echo 42".into(),
        });
        gui.handle_stream_event(StreamEvent::ToolCallEnd {
            turn: 1,
            tool: "Bash".into(),
            output: "42".into(),
            is_error: false,
        });

        // 5. Model text response (markdown), after the tool result.
        gui.handle_stream_event(StreamEvent::Text {
            turn: 1,
            text: "I've picked a number between 1 and 100.".into(),
        });

        // 6. Turn end
        gui.handle_stream_event(StreamEvent::TurnEnd {
            turn: 1,
            finish_reason: "stop".into(),
            total_tokens: 150,
            prompt_cache_hit_tokens: 0,
            prompt_cache_miss_tokens: 0,
        });

        let blocks = gui.transcript.blocks();
        assert_eq!(
            blocks.len(),
            4,
            "expected 4 blocks: the user message, the reasoning reply, \
             the tool call, and the post-tool reply"
        );
        assert_eq!(
            blocks[0].kind,
            BlockKind::User {
                text: "pick a number between 1 and 100 but don't tell me".into(),
            },
            "user input must be a User block, never assistant output"
        );
        assert_eq!(
            blocks[1].kind,
            BlockKind::Assistant {
                spans: vec![Span::Reasoning(
                    "The user wants me to pick a secret number. I'll pick 42.".into()
                )],
            },
            "reasoning before the tool call must be its own Assistant block"
        );
        assert_eq!(
            blocks[2].kind,
            BlockKind::ToolCall {
                tool: "Bash".into(),
                args: "echo 42".into(),
                output: Some("42".into()),
                is_error: false,
            },
            "the tool call block must carry the filled-in output the scripted end event sent"
        );
        assert_eq!(
            blocks[3].kind,
            BlockKind::Assistant {
                spans: vec![Span::Text("I've picked a number between 1 and 100.".into())],
            },
            "reply text after a tool result must start a new Assistant block, not join the one before the call"
        );
    }

    /// Pins the ordering rule on its own, isolated from reasoning: text
    /// that arrives after a tool call's result must start a brand new
    /// Assistant block rather than being appended to the block that was
    /// open before the call. A refactor that made the tool call transparent
    /// to span coalescing would merge "before" and "after" into one span
    /// of one block; this test fails the moment that happens.
    #[test]
    fn text_after_a_tool_call_starts_a_new_assistant_block() {
        let mut gui = make_gui();
        gui.handle_stream_event(StreamEvent::Text {
            turn: 1,
            text: "before".into(),
        });
        gui.handle_stream_event(StreamEvent::ToolCallStart {
            turn: 1,
            tool: "Bash".into(),
            args: "ls".into(),
        });
        gui.handle_stream_event(StreamEvent::ToolCallEnd {
            turn: 1,
            tool: "Bash".into(),
            output: "file1".into(),
            is_error: false,
        });
        gui.handle_stream_event(StreamEvent::Text {
            turn: 1,
            text: "after".into(),
        });

        let blocks = gui.transcript.blocks();
        assert_eq!(
            blocks.len(),
            3,
            "expected the before-text block, the tool call, and a new after-text block"
        );
        assert_eq!(
            blocks[0].kind,
            BlockKind::Assistant {
                spans: vec![Span::Text("before".into())],
            }
        );
        assert!(
            matches!(blocks[1].kind, BlockKind::ToolCall { .. }),
            "the middle block must be the tool call"
        );
        assert_eq!(
            blocks[2].kind,
            BlockKind::Assistant {
                spans: vec![Span::Text("after".into())],
            },
            "the after-text must be its own span in its own block, not merged with \"before\""
        );
        assert_ne!(
            blocks[0].id, blocks[2].id,
            "the two Assistant blocks around the tool call must be distinct blocks"
        );
    }

    #[test]
    fn turn_end_accumulates_cache_tokens() {
        let mut gui = make_gui();
        assert_eq!(gui.total_cache_hit_tokens, 0);
        assert_eq!(gui.total_cache_miss_tokens, 0);

        // First turn: 100 cache hit, 20 miss
        gui.handle_stream_event(StreamEvent::TurnEnd {
            turn: 1,
            finish_reason: "stop".into(),
            total_tokens: 150,
            prompt_cache_hit_tokens: 100,
            prompt_cache_miss_tokens: 20,
        });
        assert_eq!(gui.total_cache_hit_tokens, 100);
        assert_eq!(gui.total_cache_miss_tokens, 20);

        // Second turn: 80 cache hit, 30 miss
        gui.handle_stream_event(StreamEvent::TurnEnd {
            turn: 2,
            finish_reason: "stop".into(),
            total_tokens: 200,
            prompt_cache_hit_tokens: 80,
            prompt_cache_miss_tokens: 30,
        });
        assert_eq!(gui.total_cache_hit_tokens, 180);
        assert_eq!(gui.total_cache_miss_tokens, 50);

        // Third turn: no cache (new conversation or first turn)
        gui.handle_stream_event(StreamEvent::TurnEnd {
            turn: 3,
            finish_reason: "stop".into(),
            total_tokens: 100,
            prompt_cache_hit_tokens: 0,
            prompt_cache_miss_tokens: 0,
        });
        assert_eq!(gui.total_cache_hit_tokens, 180);
        assert_eq!(gui.total_cache_miss_tokens, 50);
    }

    #[test]
    fn with_voice_attaches_both_channels() {
        let gui = make_gui();
        let (_tx_voice_events, rx_voice) = mpsc::unbounded_channel::<VoiceEvent>();
        let (tx_voice_cmd, _rx_voice_cmd) = mpsc::unbounded_channel::<VoiceCommand>();
        let gui = gui.with_voice(rx_voice, tx_voice_cmd);
        assert!(gui.voice.is_attached());
    }

    #[test]
    fn new_seeds_every_panel_control_from_settings() {
        let gui = make_gui_with_settings(&settings_with_voice("am_michael"));
        assert!(gui.show_raw_output);
        // The voice controls seed inside `VoiceUi::new`, covered by
        // `voice_ui::tests::new_seeds_every_control_from_settings`.
    }

    #[test]
    fn transcript_event_submits_through_the_enter_path() {
        let mut gui = make_gui();
        gui.handle_voice_event_for_test(VoiceEvent::Transcript("turn on the lights".into()));
        let blocks = gui.transcript.blocks();
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            blocks[0].kind,
            BlockKind::User {
                text: "turn on the lights".into(),
            }
        );
        assert!(
            gui.input_buffer.is_empty(),
            "buffer clears on submit, same as Enter"
        );
        assert_eq!(gui.session_status, "Running...");
    }

    #[test]
    fn transcript_event_forwards_text_to_the_agent_channel() {
        let (_tx_events, rx_events) = mpsc::unbounded_channel();
        let (tx_input, mut rx_input) = mpsc::unbounded_channel();
        let mut gui = DeepSeekGui::new(
            rx_events,
            tx_input,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU8::new(0)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicUsize::new(100_000)),
            Arc::new(Mutex::new("deepseek-v4-flash".into())),
            Arc::new(Mutex::new(PathBuf::from("."))),
            Settings::default(),
            unique_temp_dir("ctor"),
        );
        gui.handle_voice_event_for_test(VoiceEvent::Transcript("hello".into()));
        match rx_input.try_recv().unwrap() {
            AgentCommand::UserTurn { text, .. } => assert_eq!(text, "hello"),
            other => panic!("expected UserTurn, got {other:?}"),
        }
    }

    #[test]
    fn empty_transcript_is_not_submitted() {
        let mut gui = make_gui();
        gui.handle_voice_event_for_test(VoiceEvent::Transcript("".into()));
        assert!(gui.transcript.blocks().is_empty());
        assert_eq!(gui.session_status, "Ready");
    }

    #[test]
    fn whitespace_only_transcript_is_not_submitted() {
        let mut gui = make_gui();
        gui.handle_voice_event_for_test(VoiceEvent::Transcript("   ".into()));
        assert!(gui.transcript.blocks().is_empty());
        assert_eq!(gui.session_status, "Ready");
    }

    #[test]
    fn voice_mode_flag_starts_matching_initial_tts_state() {
        let gui = make_gui();
        assert!(!gui.voice.tts_enabled_for_test());
        assert!(!gui.voice_mode_flag.load(Ordering::SeqCst));
    }

    #[test]
    fn voice_mode_flag_tracks_tts_toggling_on_then_off() {
        let mut gui = make_gui();

        gui.voice.set_tts_enabled_for_test(true);
        gui.voice_mode_flag.store(
            voice_mode_flag_for_tts(gui.voice.tts_enabled_for_test()),
            Ordering::SeqCst,
        );
        assert!(gui.voice_mode_flag.load(Ordering::SeqCst));

        gui.voice.set_tts_enabled_for_test(false);
        gui.voice_mode_flag.store(
            voice_mode_flag_for_tts(gui.voice.tts_enabled_for_test()),
            Ordering::SeqCst,
        );
        assert!(!gui.voice_mode_flag.load(Ordering::SeqCst));
    }

    /// A GUI with the voice command channel attached, plus the receiving
    /// end of that channel so a test can see what got sent.
    fn make_gui_with_voice() -> (DeepSeekGui, mpsc::UnboundedReceiver<VoiceCommand>) {
        let gui = make_gui();
        let (_tx_voice_events, rx_voice) = mpsc::unbounded_channel::<VoiceEvent>();
        let (tx_voice_cmd, rx_voice_cmd) = mpsc::unbounded_channel::<VoiceCommand>();
        let gui = gui.with_voice(rx_voice, tx_voice_cmd);
        (gui, rx_voice_cmd)
    }

    #[test]
    fn turn_end_speaks_the_accumulated_reply_when_tts_is_enabled() {
        let (mut gui, mut rx_voice_cmd) = make_gui_with_voice();
        gui.voice.set_tts_enabled_for_test(true);

        gui.handle_stream_event(StreamEvent::Text {
            turn: 1,
            text: "Run `cargo test` to check it.".into(),
        });
        gui.handle_stream_event(StreamEvent::TurnEnd {
            turn: 1,
            finish_reason: "stop".into(),
            total_tokens: 10,
            prompt_cache_hit_tokens: 0,
            prompt_cache_miss_tokens: 0,
        });

        assert_eq!(
            rx_voice_cmd.try_recv().unwrap(),
            VoiceCommand::Speak("Run to check it.".into())
        );
        assert!(gui.voice.reply_buffer_for_test().is_empty());
    }

    #[test]
    fn turn_end_sends_nothing_when_tts_is_disabled() {
        let (mut gui, mut rx_voice_cmd) = make_gui_with_voice();
        assert!(!gui.voice.tts_enabled_for_test(), "tts is off by default");

        gui.handle_stream_event(StreamEvent::Text {
            turn: 1,
            text: "Hello there.".into(),
        });
        gui.handle_stream_event(StreamEvent::TurnEnd {
            turn: 1,
            finish_reason: "stop".into(),
            total_tokens: 10,
            prompt_cache_hit_tokens: 0,
            prompt_cache_miss_tokens: 0,
        });

        assert!(rx_voice_cmd.try_recv().is_err());
    }

    #[test]
    fn turn_end_sends_exactly_one_speak_for_multiple_text_chunks() {
        let (mut gui, mut rx_voice_cmd) = make_gui_with_voice();
        gui.voice.set_tts_enabled_for_test(true);

        gui.handle_stream_event(StreamEvent::Text {
            turn: 1,
            text: "Hello".into(),
        });
        gui.handle_stream_event(StreamEvent::Text {
            turn: 1,
            text: " there.".into(),
        });
        gui.handle_stream_event(StreamEvent::TurnEnd {
            turn: 1,
            finish_reason: "stop".into(),
            total_tokens: 10,
            prompt_cache_hit_tokens: 0,
            prompt_cache_miss_tokens: 0,
        });

        assert_eq!(
            rx_voice_cmd.try_recv().unwrap(),
            VoiceCommand::Speak("Hello there.".into())
        );
        assert!(
            rx_voice_cmd.try_recv().is_err(),
            "exactly one Speak per turn, not one per chunk"
        );
    }

    #[test]
    fn reasoning_and_tool_output_are_never_spoken() {
        let (mut gui, mut rx_voice_cmd) = make_gui_with_voice();
        gui.voice.set_tts_enabled_for_test(true);

        gui.handle_stream_event(StreamEvent::Reasoning {
            turn: 1,
            text: "thinking about it".into(),
        });
        gui.handle_stream_event(StreamEvent::ToolCallStart {
            turn: 1,
            tool: "Bash".into(),
            args: "{}".into(),
        });
        gui.handle_stream_event(StreamEvent::ToolCallEnd {
            turn: 1,
            tool: "Bash".into(),
            output: "some tool output".into(),
            is_error: false,
        });
        gui.handle_stream_event(StreamEvent::TurnEnd {
            turn: 1,
            finish_reason: "stop".into(),
            total_tokens: 10,
            prompt_cache_hit_tokens: 0,
            prompt_cache_miss_tokens: 0,
        });

        assert!(
            rx_voice_cmd.try_recv().is_err(),
            "no model reply text was seen, so nothing should be spoken"
        );
    }

    #[test]
    fn interrupted_clears_the_pending_reply_so_it_is_never_spoken_later() {
        let (mut gui, mut rx_voice_cmd) = make_gui_with_voice();
        gui.voice.set_tts_enabled_for_test(true);

        gui.handle_stream_event(StreamEvent::Text {
            turn: 1,
            text: "partial reply".into(),
        });
        gui.handle_stream_event(StreamEvent::Interrupted {
            message: "Interrupted by user (Escape)".into(),
        });
        assert!(gui.voice.reply_buffer_for_test().is_empty());

        // The next turn must not inherit the interrupted turn's text.
        gui.handle_stream_event(StreamEvent::Text {
            turn: 2,
            text: "fresh reply".into(),
        });
        gui.handle_stream_event(StreamEvent::TurnEnd {
            turn: 2,
            finish_reason: "stop".into(),
            total_tokens: 10,
            prompt_cache_hit_tokens: 0,
            prompt_cache_miss_tokens: 0,
        });

        assert_eq!(
            rx_voice_cmd.try_recv().unwrap(),
            VoiceCommand::Speak("fresh reply".into())
        );
    }

    #[test]
    fn new_gui_seeds_context_budget_from_the_flag() {
        let (_tx_events, rx_events) = mpsc::unbounded_channel();
        let (tx_input, _rx_input) = mpsc::unbounded_channel();
        let gui = DeepSeekGui::new(
            rx_events,
            tx_input,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU8::new(0)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicUsize::new(64_000)),
            Arc::new(Mutex::new("deepseek-v4-flash".into())),
            Arc::new(Mutex::new(PathBuf::from("."))),
            Settings::default(),
            unique_temp_dir("ctor"),
        );
        assert_eq!(gui.context_budget, 64_000);
    }

    #[test]
    fn context_budget_flag_write_is_observable_through_a_second_handle() {
        let mut gui = make_gui();
        let observer = Arc::clone(&gui.context_budget_flag);
        gui.context_budget = 80_000;
        gui.context_budget_flag.store(80_000, Ordering::SeqCst);
        assert_eq!(observer.load(Ordering::SeqCst), 80_000);
    }

    #[test]
    fn context_budget_caption_value_is_a_third_of_the_budget() {
        let mut gui = make_gui();
        gui.context_budget = 90_000;
        assert_eq!(gui.context_budget / 3, 30_000);
    }

    #[test]
    fn new_gui_seeds_effort_from_the_flag() {
        let (tx_events, rx_events) = mpsc::unbounded_channel();
        let (tx_input, _rx_input) = mpsc::unbounded_channel();
        let effort_flag = Arc::new(AtomicU8::new(0));
        Effort::High.store(&effort_flag);
        let gui = DeepSeekGui::new(
            rx_events,
            tx_input,
            Arc::new(AtomicBool::new(false)),
            effort_flag,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicUsize::new(64_000)),
            Arc::new(Mutex::new("deepseek-v4-flash".into())),
            Arc::new(Mutex::new(PathBuf::from("."))),
            Settings::default(),
            unique_temp_dir("effort-ctor"),
        );
        let _ = tx_events;
        assert_eq!(gui.effort, Effort::High);
    }

    #[test]
    fn new_gui_seeds_effort_from_settings_via_the_test_helper() {
        let settings = Settings {
            effort: Some(Effort::Max),
            ..Settings::default()
        };
        let gui = make_gui_with_settings(&settings);
        assert_eq!(gui.effort, Effort::Max);
    }

    #[test]
    fn effort_flag_write_is_observable_through_a_second_handle() {
        let mut gui = make_gui();
        let observer = Arc::clone(&gui.effort_flag);
        gui.effort = Effort::Medium;
        gui.effort.store(&gui.effort_flag);
        assert_eq!(Effort::load(&observer), Effort::Medium);
    }

    #[test]
    fn apply_effort_round_trips_through_settings() {
        let mut settings = Settings::default();
        assert_eq!(settings.effort(), Effort::None);
        apply_effort(&mut settings, Effort::Low);
        assert_eq!(settings.effort(), Effort::Low);
    }

    #[test]
    fn apply_effort_reaches_every_level() {
        let mut settings = Settings::default();
        for level in [
            Effort::None,
            Effort::Low,
            Effort::Medium,
            Effort::High,
            Effort::Max,
        ] {
            apply_effort(&mut settings, level);
            assert_eq!(settings.effort(), level);
            assert_eq!(settings.effort().to_u8(), level.to_u8());
        }
    }

    #[test]
    fn apply_effort_persists_to_disk() {
        let project_root = unique_temp_dir("effort-persist");
        let mut gui = make_gui_in(&Settings::default(), project_root.clone());
        apply_effort(&mut gui.settings, Effort::High);
        gui.persist_settings();
        let reloaded = Settings::load(&project_root).unwrap();
        assert_eq!(reloaded.effort(), Effort::High);
    }

    /// Apply a change to a GUI's settings, persist it, and read the file
    /// back through the real load path. This is the seam every panel
    /// control goes through.
    fn round_trip<F: FnOnce(&mut Settings)>(change: F) -> Settings {
        let dir = unique_temp_dir("persist");
        let gui = make_gui_in(&Settings::default(), dir.clone());
        let mut gui = gui;
        change(&mut gui.settings);
        gui.persist_settings();
        assert!(dir.join("settings.json").exists(), "the file must be there");
        let loaded = Settings::load(&dir).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        loaded
    }

    #[test]
    fn default_backend_change_survives_a_save_and_a_load() {
        let loaded = round_trip(|s| apply_default_backend(s, "claude"));
        assert_eq!(loaded.default_backend(), Some("claude"));
    }

    #[test]
    fn show_raw_output_change_survives_a_save_and_a_load() {
        let loaded = round_trip(|s| apply_show_raw_output(s, true));
        assert!(loaded.show_raw_output());
    }

    #[test]
    fn context_budget_change_survives_a_save_and_a_load() {
        let loaded = round_trip(|s| apply_context_budget(s, 150_000));
        assert_eq!(loaded.context_budget(), 150_000);
    }

    #[test]
    fn new_gui_seeds_working_dir_buffer_from_the_flag() {
        let (_tx_events, rx_events) = mpsc::unbounded_channel();
        let (tx_input, _rx_input) = mpsc::unbounded_channel();
        let seeded_dir = unique_temp_dir("seed-workdir");
        let gui = DeepSeekGui::new(
            rx_events,
            tx_input,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU8::new(0)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicUsize::new(64_000)),
            Arc::new(Mutex::new("deepseek-v4-flash".into())),
            Arc::new(Mutex::new(seeded_dir.clone())),
            Settings::default(),
            unique_temp_dir("ctor"),
        );
        assert_eq!(gui.working_dir_buffer, seeded_dir.display().to_string());
    }

    #[test]
    fn working_dir_change_survives_a_save_and_a_load() {
        let dir = unique_temp_dir("workdir-persist");
        let loaded = round_trip(|s| apply_working_dir(s, &dir.display().to_string()));
        assert_eq!(loaded.working_dir(), Some(dir.display().to_string()));
    }

    #[test]
    fn commit_working_dir_change_writes_a_valid_directory_to_the_shared_flag() {
        let mut gui = make_gui();
        let dir = unique_temp_dir("commit-valid");
        gui.working_dir_buffer = dir.display().to_string();

        gui.commit_working_dir_change();

        assert_eq!(*gui.working_dir_flag.lock().unwrap(), dir);
        assert_eq!(gui.settings.working_dir(), Some(dir.display().to_string()));
    }

    #[test]
    fn commit_working_dir_change_rejects_a_path_that_is_not_a_directory() {
        let mut gui = make_gui();
        let original = gui.working_dir_flag.lock().unwrap().clone();
        gui.working_dir_buffer = "Z:/definitely/does/not/exist/anywhere".to_string();

        gui.commit_working_dir_change();

        assert_eq!(*gui.working_dir_flag.lock().unwrap(), original);
        assert!(gui.settings.working_dir().is_none());
    }

    #[test]
    fn voice_change_creates_the_block_when_it_is_missing() {
        let mut settings = Settings::default();
        assert!(settings.voice.is_none(), "no voice block to start");
        apply_stt_enabled(&mut settings, true);
        assert!(settings.voice.is_some(), "the block gets created");
        assert!(settings.voice.as_ref().unwrap().stt_enabled);
    }

    #[test]
    fn every_voice_control_change_survives_a_save_and_a_load() {
        let loaded = round_trip(|s| {
            apply_voice_enabled(s, true);
            apply_stt_enabled(s, true);
            apply_tts_enabled(s, true);
            apply_trigger_mode(s, TriggerMode::WakeWord);
            apply_wake_phrase(s, "hey computer");
            apply_tts_voice(s, "am_michael");
            apply_tts_speed(s, 1.4);
        });
        assert!(loaded.voice_enabled());
        assert!(loaded.voice_stt_enabled());
        assert!(loaded.voice_tts_enabled());
        assert_eq!(loaded.voice_trigger_mode(), TriggerMode::WakeWord);
        assert_eq!(loaded.voice_wake_phrase(), "hey computer");
        assert_eq!(loaded.voice_tts_voice(), "am_michael");
        assert_eq!(loaded.voice_tts_speed(), 1.4);
    }

    #[test]
    fn persisting_to_an_unwritable_root_logs_instead_of_panicking() {
        let gui = make_gui_in(
            &Settings::default(),
            PathBuf::from("/nonexistent/dsc/path/xyz"),
        );
        // Must not panic. The failure is logged and the session goes on.
        gui.persist_settings();
    }

    #[test]
    fn active_tab_defaults_to_chat() {
        let gui = make_gui();
        assert_eq!(gui.active_tab, ActiveTab::Chat);
    }

    #[test]
    fn autopilot_task_change_survives_a_save_and_a_load() {
        let loaded = round_trip(|s| apply_autopilot_task(s, "fix the build"));
        assert_eq!(loaded.autopilot_task().as_deref(), Some("fix the build"));
    }

    #[test]
    fn autopilot_iterations_change_survives_a_save_and_a_load() {
        let loaded = round_trip(|s| apply_autopilot_iterations(s, 12));
        assert_eq!(loaded.autopilot_iterations(), 12);
    }

    #[test]
    fn autopilot_progress_updates_from_iteration_start_then_finished() {
        let mut gui = make_gui();
        assert_eq!(gui.autopilot.progress(), AutopilotProgress::Idle);

        gui.handle_stream_event(StreamEvent::RepeatIterationStart { index: 2, total: 5 });
        assert_eq!(
            gui.autopilot.progress(),
            AutopilotProgress::Running { index: 2, total: 5 }
        );

        gui.handle_stream_event(StreamEvent::RepeatFinished {
            completed: 5,
            total: 5,
        });
        assert_eq!(
            gui.autopilot.progress(),
            AutopilotProgress::Finished {
                completed: 5,
                total: 5
            }
        );
    }

    #[test]
    fn resolve_autopilot_policy_path_defaults_under_project_root() {
        let root = PathBuf::from("/project");
        let store = crate::autopilot::policy::PolicyStore::new(root.clone(), None);
        let path = store.resolved_policy_path();
        assert_eq!(path, root.join("autopilot-policy.md"));
    }

    #[test]
    fn resolve_autopilot_policy_path_resolves_relative_override_against_root() {
        let root = PathBuf::from("/project");
        let store = crate::autopilot::policy::PolicyStore::new(
            root.clone(),
            Some("custom-policy.md".into()),
        );
        let path = store.resolved_policy_path();
        assert_eq!(path, root.join("custom-policy.md"));
    }

    #[test]
    fn session_reset_clears_the_pending_reply() {
        let mut gui = make_gui();
        gui.handle_stream_event(StreamEvent::Text {
            turn: 1,
            text: "partial reply".into(),
        });
        gui.handle_stream_event(StreamEvent::SessionReset);
        assert!(gui.voice.reply_buffer_for_test().is_empty());
    }

    fn user_message(text: &str) -> Message {
        Message {
            role: Role::User,
            content: Some(Content::text(text)),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    /// Drive one full turn through a GUI: a user block in the transcript,
    /// a `ConversationSnapshot` carrying `messages`, then `TurnEnd`, which
    /// triggers the autosave.
    fn run_one_turn(gui: &mut DeepSeekGui, user_text: &str) {
        gui.transcript.push(BlockKind::User {
            text: user_text.into(),
        });
        gui.handle_stream_event(StreamEvent::ConversationSnapshot {
            messages: vec![user_message(user_text)],
            claude_session_id: None,
        });
        gui.handle_stream_event(StreamEvent::TurnEnd {
            turn: 1,
            finish_reason: "stop".into(),
            total_tokens: 10,
            prompt_cache_hit_tokens: 0,
            prompt_cache_miss_tokens: 0,
        });
    }

    #[test]
    fn turn_end_writes_a_session_file_the_store_can_load_back() {
        let mut gui = make_gui();
        run_one_turn(&mut gui, "fix the parser bug");

        let loaded = gui
            .sessions
            .store()
            .load(&gui.sessions.current_id())
            .expect("expected the autosaved session to load back");
        assert_eq!(loaded.messages.len(), 1);
        assert_eq!(loaded.meta.title, "fix the parser bug");
    }

    #[test]
    fn new_session_saves_outgoing_then_leaves_an_empty_transcript_and_a_different_id() {
        let mut gui = make_gui();
        run_one_turn(&mut gui, "add a new endpoint");
        let old_id = gui.sessions.current_id();

        gui.start_new_session();

        assert!(gui.sessions.store().load(&old_id).is_ok());
        assert!(gui.transcript.blocks().is_empty());
        assert_ne!(gui.sessions.current_id(), old_id);
        assert!(gui.sessions.messages().is_empty());
    }

    #[test]
    fn new_session_from_empty_conversation_writes_nothing_to_disk() {
        let mut gui = make_gui();
        let old_id = gui.sessions.current_id();

        gui.start_new_session();

        assert!(gui.sessions.store().load(&old_id).is_err());
        assert!(gui.sessions.saved().is_empty());
    }

    #[test]
    fn load_session_saves_outgoing_then_installs_the_loaded_transcript_and_id() {
        let mut gui = make_gui();
        run_one_turn(&mut gui, "first conversation");
        let first_id = gui.sessions.current_id();

        gui.start_new_session();
        run_one_turn(&mut gui, "second conversation");
        let second_id = gui.sessions.current_id();

        gui.load_session(first_id);

        assert_eq!(gui.sessions.current_id(), first_id);
        assert_eq!(gui.transcript.blocks().len(), 1);
        let saved_second = gui
            .sessions
            .store()
            .load(&second_id)
            .expect("expected the outgoing second conversation to be saved");
        assert_eq!(saved_second.meta.title, "second conversation");
    }

    #[test]
    fn session_reset_saves_the_outgoing_conversation_rather_than_discarding_it() {
        let mut gui = make_gui();
        run_one_turn(&mut gui, "reset me please");
        let old_id = gui.sessions.current_id();

        gui.handle_stream_event(StreamEvent::SessionReset);

        let loaded = gui
            .sessions
            .store()
            .load(&old_id)
            .expect("expected the pre-reset conversation to have been saved");
        assert_eq!(loaded.meta.title, "reset me please");
        assert_ne!(gui.sessions.current_id(), old_id);
    }

    #[test]
    fn a_save_failure_does_not_panic_and_does_not_take_down_the_session() {
        // Point the session store at a path that cannot be a directory: a
        // regular file sits where the sessions directory would need to go,
        // so `save`'s `create_dir_all` fails every time.
        let root = unique_temp_dir("save-failure");
        std::fs::write(root.join(".deepseek"), "not a directory").unwrap();
        let mut gui = make_gui_in(&Settings::default(), root);

        run_one_turn(&mut gui, "this save will fail");

        assert_eq!(gui.session_status, "Ready");
    }

    #[test]
    fn title_is_derived_on_first_turn_and_not_rederived_once_set() {
        let mut gui = make_gui();
        run_one_turn(&mut gui, "the original title");
        assert_eq!(gui.sessions.title_for_test(), "the original title");

        gui.handle_stream_event(StreamEvent::ConversationSnapshot {
            messages: vec![
                user_message("the original title"),
                user_message("a later message that should not overwrite the title"),
            ],
            claude_session_id: None,
        });
        gui.handle_stream_event(StreamEvent::TurnEnd {
            turn: 2,
            finish_reason: "stop".into(),
            total_tokens: 20,
            prompt_cache_hit_tokens: 0,
            prompt_cache_miss_tokens: 0,
        });

        assert_eq!(gui.sessions.title_for_test(), "the original title");
    }

    #[test]
    fn switching_to_sessions_tab_does_not_disturb_the_transcript() {
        let mut gui = make_gui();
        run_one_turn(&mut gui, "keep this transcript intact");
        let block_count_before = gui.transcript.blocks().len();

        gui.active_tab = ActiveTab::Sessions;

        assert_eq!(gui.active_tab, ActiveTab::Sessions);
        assert_eq!(gui.transcript.blocks().len(), block_count_before);
    }

    #[test]
    fn new_chat_action_leaves_an_empty_transcript_and_returns_to_chat_tab() {
        let mut gui = make_gui();
        run_one_turn(&mut gui, "an old conversation");
        gui.active_tab = ActiveTab::Sessions;

        // This mirrors exactly what the Sessions tab's "New Chat" button
        // does: start a fresh session, then switch the view back to Chat.
        gui.start_new_session();
        gui.active_tab = ActiveTab::Chat;

        assert!(gui.transcript.blocks().is_empty());
        assert_eq!(gui.active_tab, ActiveTab::Chat);
    }

    #[test]
    fn deleting_a_session_removes_it_from_the_list() {
        let mut gui = make_gui();
        run_one_turn(&mut gui, "a session to delete");
        let id = gui.sessions.current_id();
        assert!(gui.sessions.saved().iter().any(|meta| meta.id == id));

        gui.delete_saved_session(id);

        assert!(!gui.sessions.saved().iter().any(|meta| meta.id == id));
    }

    // ── Routed events (P2S03) ──

    fn test_route_hop(id: SubagentId) -> RouteHop {
        RouteHop {
            id,
            meta: SubagentMeta {
                backend: "ollama".into(),
                model: "test-model".into(),
                depth: 1,
            },
            session_turns: 1,
            session_turn_cap: 20,
            send_message_calls: 0,
            send_message_call_cap: 10,
        }
    }

    /// A routed event with a non-empty route lands inside its own nested
    /// `Subagent` block, not as a top-level block in the main transcript.
    #[test]
    fn a_routed_event_lands_in_a_nested_subagent_block_not_the_top_level_transcript() {
        let mut gui = make_gui();
        let subagent_id = SubagentId::next();

        gui.handle_routed_event(RoutedEvent {
            route: vec![test_route_hop(subagent_id)],
            event: StreamEvent::Text {
                turn: 1,
                text: "hi from a subagent".into(),
            },
        });

        let blocks = gui.transcript.blocks();
        assert_eq!(blocks.len(), 1);
        let BlockKind::Subagent { transcript, .. } = &blocks[0].kind else {
            panic!("expected a Subagent block, got {:?}", blocks[0].kind);
        };
        assert_eq!(
            transcript.blocks()[0].kind,
            BlockKind::Assistant {
                spans: vec![Span::Text("hi from a subagent".into())],
            }
        );
    }

    /// A subagent's own `TurnEnd`, arriving with a non-empty route, must
    /// not trigger any of the main session's `TurnEnd` side effects: no
    /// token-count update, no cache-counter update, no session autosave.
    #[test]
    fn a_subagent_turn_end_does_not_touch_main_session_counters_or_trigger_a_save() {
        let mut gui = make_gui();
        run_one_turn(&mut gui, "the real conversation");
        let token_count_before = gui.token_count.clone();
        let hit_before = gui.total_cache_hit_tokens;
        let miss_before = gui.total_cache_miss_tokens;
        let status_before = gui.session_status.clone();
        let saved_sessions_before = gui.sessions.saved().len();
        let subagent_id = SubagentId::next();

        gui.handle_routed_event(RoutedEvent {
            route: vec![test_route_hop(subagent_id)],
            event: StreamEvent::TurnEnd {
                turn: 1,
                finish_reason: "stop".into(),
                total_tokens: 9999,
                prompt_cache_hit_tokens: 500,
                prompt_cache_miss_tokens: 500,
            },
        });

        assert_eq!(gui.token_count, token_count_before);
        assert_eq!(gui.total_cache_hit_tokens, hit_before);
        assert_eq!(gui.total_cache_miss_tokens, miss_before);
        assert_eq!(gui.session_status, status_before);
        assert_eq!(gui.sessions.saved().len(), saved_sessions_before);
    }

    /// A main-session event, empty route, still does everything it always
    /// did: `handle_routed_event` with an empty route behaves exactly like
    /// the old `handle_stream_event` path, side effects included.
    #[test]
    fn a_main_session_routed_event_still_applies_its_side_effects() {
        let mut gui = make_gui();
        assert_eq!(gui.total_cache_hit_tokens, 0);

        gui.handle_routed_event(RoutedEvent::own(StreamEvent::TurnEnd {
            turn: 1,
            finish_reason: "stop".into(),
            total_tokens: 42,
            prompt_cache_hit_tokens: 10,
            prompt_cache_miss_tokens: 5,
        }));

        assert_eq!(gui.token_count, "42");
        assert_eq!(gui.total_cache_hit_tokens, 10);
        assert_eq!(gui.total_cache_miss_tokens, 5);
        // And the transcript still sees it too, exactly as
        // `apply_stream_event` would have handled it directly.
        assert!(gui.transcript.blocks().is_empty());
    }
}
