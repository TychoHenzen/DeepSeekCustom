pub mod agent_handles;
pub mod attachment;
pub mod autopilot_tab;
pub mod backend_picker;
pub mod session_state;
pub mod sessions_tab;
pub mod settings_panel;
pub mod transcript;
pub mod voice_ui;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use eframe::App;
use eframe::egui::{self, Color32, RichText, ScrollArea, TextEdit};
use egui_commonmark::CommonMarkCache;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use self::agent_handles::AgentHandles;
use self::attachment::{AttachmentSlot, decode_image_bytes};
use self::autopilot_tab::AutopilotTab;
use self::backend_picker::BackendPicker;
use self::session_state::{SessionOrigin, SessionState};
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
pub enum ActiveTab {
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
    token_count: String,
    session_status: String,
    /// Cumulative prompt cache hit tokens (not charged as input).
    total_cache_hit_tokens: u32,
    /// Cumulative prompt cache miss tokens (charged as input).
    total_cache_miss_tokens: u32,
    rx_events: mpsc::UnboundedReceiver<RoutedEvent>,
    tx_input: mpsc::UnboundedSender<AgentCommand>,
    auto_scroll: bool,

    // ── Settings panel ──
    /// The handles the agent re-reads on its own schedule. See
    /// `gui::agent_handles`.
    handles: AgentHandles,

    settings_visible: bool,
    /// Sorted backend names, the keys of `settings.backends()`.
    /// The backend and model dropdowns, the running backend's name and
    /// model, and the background model discovery. See
    /// `gui::backend_picker`.
    backends: BackendPicker,

    /// Sidebar combo box's current selection, seeded from `effort_flag` in
    /// `new`. Mirrors `context_budget`: the flag is what the agent reads
    /// each turn, this field is what the control renders and edits.
    effort: Effort,
    /// Slider's current value, seeded from `context_budget_flag` in `new`.
    context_budget: usize,
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
        handles: AgentHandles,
        settings: Settings,
        project_root: PathBuf,
    ) -> Self {
        let backends = BackendPicker::new(&settings, Arc::clone(&handles.model));
        let voice = VoiceUi::new(&settings, &handles.voice_mode);
        let context_budget = handles.context_budget.load(Ordering::SeqCst);
        let effort = Effort::load(&handles.effort);
        // Seed the text field from the shared handle, which `main.rs`
        // already resolved against a saved `working_dir` setting (falling
        // back to `project_root` when that setting was absent or no longer
        // a real directory). Reading it back here, rather than reading
        // `settings.working_dir()` a second time, keeps this single source
        // of truth: the buffer always starts equal to what the tools will
        // actually act against.
        let working_dir_buffer = handles.working_dir.lock().unwrap().display().to_string();
        let autopilot = AutopilotTab::new(&settings, &project_root);
        let sessions = SessionState::new(
            SessionStore::for_project(&project_root),
            SessionOrigin {
                backend: backends.active_backend().to_string(),
                model: backends.model().to_string(),
            },
        );
        Self {
            transcript: Transcript::new(),
            input_buffer: String::new(),
            token_count: "0".into(),
            session_status: "Ready".into(),
            total_cache_hit_tokens: 0,
            total_cache_miss_tokens: 0,
            rx_events,
            tx_input,
            auto_scroll: false,
            handles,
            settings_visible: false,
            backends,
            effort,
            context_budget,
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
    /// every real event arrives through `drain_events` in `update`, which
    /// needs a live `egui::Context` no test can build.
    #[cfg(feature = "test-support")]
    pub fn handle_voice_event_for_test(&mut self, event: VoiceEvent) {
        if let Some(text) = self.voice.handle_event(event, &mut self.transcript) {
            self.input_buffer = text;
            self.submit_current_input();
        }
    }

    /// Which backend and model a save should record. Built fresh on each
    /// call rather than held, since the model picker can move under it.
    fn session_origin(&self) -> SessionOrigin {
        SessionOrigin {
            backend: self.backends.active_backend().to_string(),
            model: self.backends.model().to_string(),
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
    #[cfg(feature = "test-support")]
    pub fn handle_stream_event(&mut self, event: StreamEvent) {
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
        self.render_output_heading(ui);
        ui.separator();
        self.render_transcript_blocks(ui);
    }

    /// Draw the "Output" heading row shared by the Chat and Autopilot tabs:
    /// the heading itself plus a right-aligned grey block count.
    fn render_output_heading(&self, ui: &mut egui::Ui) {
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
    }

    /// Draw the scroll area of transcript blocks. Both the Chat tab and the
    /// Autopilot tab call this to show the same transcript.
    fn render_transcript_blocks(&mut self, ui: &mut egui::Ui) {
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
        for (backend_name, models) in self.backends.drain_fetched_lists() {
            self.backends.apply_fetched_list(&backend_name, models);
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
            self.handles.interrupt.store(true, Ordering::SeqCst);
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
                    ui.separator();
                    self.render_output_heading(ui);
                    ui.separator();
                    self.render_transcript_blocks(ui);
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
                    let backend_name = self.backends.selected_name().unwrap_or("unknown");
                    ui.label(
                        RichText::new(format!(
                            "Backend: {backend_name} ({})",
                            self.backends.model()
                        ))
                        .color(Color32::from_rgb(0, 255, 255)),
                    );
                    ui.separator();
                    ui.label(
                        RichText::new(format!(
                            "Dir: {}",
                            self.handles.working_dir.lock().unwrap().display()
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
                    let effort_label = format!("Effort: {:?}", Effort::load(&self.handles.effort));
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
pub const TOOL_ERROR_COLOR: Color32 = Color32::from_rgb(255, 80, 80);
const TOOL_OUTPUT_COLOR: Color32 = Color32::from_rgb(0, 200, 0);
const REASONING_LABEL: &str = "Reasoning";
pub const IMAGE_LABEL_COLOR: Color32 = Color32::from_rgb(200, 160, 220);
/// The longer side of an inline image thumbnail in the transcript, in
/// points. Clicking it opens the same image full size in its own window.
const IMAGE_THUMBNAIL_MAX: f32 = 240.0;
/// Space above a block that opens a new turn. This is what replaced the
/// old grey "--- turn end ---" divider: a turn boundary now reads as a
/// gap between bubbles instead of a line of its own.
pub const TURN_GAP: f32 = 14.0;
pub const BLOCK_GAP: f32 = 4.0;
/// How much of a tool call's arguments a collapsed summary shows.
const COLLAPSED_ARGS_LEN: usize = 60;
/// How far a user bubble is indented, so the two sides of the
/// conversation do not share a left edge.
const USER_BUBBLE_INDENT: f32 = 48.0;

/// Colour for a notice of the given severity. These are the same colours
/// the flat-line shape used: red for an error, orange for an interrupt,
/// cyan for a session reset, grey for anything quieter.
pub fn severity_color(severity: Severity) -> Color32 {
    match severity {
        Severity::Error => Color32::from_rgb(255, 80, 80),
        Severity::Warning => Color32::from_rgb(255, 165, 0),
        Severity::Info => Color32::from_rgb(0, 255, 255),
        Severity::Debug => Color32::from_rgb(128, 128, 128),
    }
}

/// The label naming who or what produced a block.
pub fn role_label(kind: &BlockKind) -> &'static str {
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
pub fn gap_before(index: usize, kind: &BlockKind) -> f32 {
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
pub fn tool_summary(collapsed: bool, tool: &str, args: &str, is_error: bool) -> String {
    let marker = if collapsed { '\u{25b6}' } else { '\u{25bc}' };
    let error = if is_error { " [error]" } else { "" };
    let args = truncate_args(args, COLLAPSED_ARGS_LEN);
    format!("{marker} {tool}{error}  {args}")
}

/// Flatten arguments onto one line and cut them to `max` characters, so a
/// collapsed tool call stays one line however long its arguments are.
pub fn truncate_args(args: &str, max: usize) -> String {
    let flat = args.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let cut: String = flat.chars().take(max).collect();
    format!("{cut}...")
}

/// Colour of a tool call's own lines. An error is red in both the
/// collapsed and the expanded state.
pub fn tool_color(is_error: bool) -> Color32 {
    if is_error {
        TOOL_ERROR_COLOR
    } else {
        TOOL_COLOR
    }
}

/// Colour of a tool call's output body.
pub fn tool_output_color(is_error: bool) -> Color32 {
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
pub fn raw_block_text(block: &Block) -> String {
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
pub fn raw_span_text(span: &Span) -> String {
    match span {
        Span::Text(text) => text.clone(),
        Span::Reasoning(text) => format!("[reasoning] {text}"),
    }
}

/// Colour for a whole block on the raw path, chosen from its kind alone.
pub fn block_color(kind: &BlockKind) -> Color32 {
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
pub fn subagent_elapsed_ms(started_at: Option<Instant>, stored_elapsed_ms: u64) -> u64 {
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
pub fn subagent_state_color(state: SubagentState) -> Color32 {
    match state {
        SubagentState::Running => Color32::from_rgb(150, 150, 255),
        SubagentState::Done => Color32::from_rgb(100, 200, 100),
        SubagentState::Failed => TOOL_ERROR_COLOR,
        SubagentState::Interrupted => Color32::from_rgb(255, 165, 0),
    }
}

/// Format a millisecond count as seconds with one decimal place, matching
/// how the rest of this file reports durations to the user.
pub fn format_elapsed_ms(elapsed_ms: u64) -> String {
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
pub fn subagent_header_summary(
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

/// Read and write access to `DeepSeekGui`'s own state, for tests.
///
/// `DeepSeekGui` is an `eframe::App`. Its whole public surface is `new`,
/// the two `with_*` builders, and `update`, and `update` needs a live
/// `egui::Context` and a real window, which a test process has neither of.
/// Every field below is state the paint path owns and drives, so there is
/// no side-effect-free seam to read it through: the alternative to these
/// accessors is a windowed integration test, not a better API.
///
/// The whole block is gated on `#[cfg(feature = "test-support")]`,
/// matching every other test-only seam in this crate. A method the paint
/// path also calls keeps its own visibility and gets a `_for_test` wrapper
/// here instead of being opened up.
#[cfg(feature = "test-support")]
impl DeepSeekGui {
    /// The Chat tab's block history.
    pub fn transcript_for_test(&self) -> &Transcript {
        &self.transcript
    }

    /// The Chat tab's block history, for a test that seeds a block
    /// directly rather than through an event.
    pub fn transcript_mut_for_test(&mut self) -> &mut Transcript {
        &mut self.transcript
    }

    /// The text the next submission will send.
    pub fn input_buffer_for_test(&self) -> &str {
        &self.input_buffer
    }

    /// Fill the input box, the way typing into it does.
    pub fn set_input_buffer_for_test(&mut self, text: &str) {
        self.input_buffer = text.to_string();
    }

    /// The status bar's session state word.
    pub fn session_status_for_test(&self) -> &str {
        &self.session_status
    }

    /// The status bar's token readout.
    pub fn token_count_for_test(&self) -> &str {
        &self.token_count
    }

    /// The running prompt cache hit total.
    pub fn total_cache_hit_tokens_for_test(&self) -> u32 {
        self.total_cache_hit_tokens
    }

    /// The running prompt cache miss total.
    pub fn total_cache_miss_tokens_for_test(&self) -> u32 {
        self.total_cache_miss_tokens
    }

    /// Replace the agent command channel, so a test can read back what a
    /// submission sent.
    pub fn set_tx_input_for_test(&mut self, tx_input: mpsc::UnboundedSender<AgentCommand>) {
        self.tx_input = tx_input;
    }

    /// The handles shared with the agent.
    pub fn handles_for_test(&self) -> &AgentHandles {
        &self.handles
    }

    /// The backend and model pickers.
    pub fn backends_mut_for_test(&mut self) -> &mut BackendPicker {
        &mut self.backends
    }

    /// The effort combo box's selection.
    pub fn effort_for_test(&self) -> Effort {
        self.effort
    }

    /// Set the effort combo box's selection.
    pub fn set_effort_for_test(&mut self, effort: Effort) {
        self.effort = effort;
    }

    /// The context budget slider's value.
    pub fn context_budget_for_test(&self) -> usize {
        self.context_budget
    }

    /// Set the context budget slider's value.
    pub fn set_context_budget_for_test(&mut self, budget: usize) {
        self.context_budget = budget;
    }

    /// The working directory field's text.
    pub fn working_dir_buffer_for_test(&self) -> &str {
        &self.working_dir_buffer
    }

    /// Set the working directory field's text, the way typing into it
    /// does. Nothing is committed until `commit_working_dir_change_for_test`.
    pub fn set_working_dir_buffer_for_test(&mut self, dir: &str) {
        self.working_dir_buffer = dir.to_string();
    }

    /// The raw-output checkbox's value.
    pub fn show_raw_output_for_test(&self) -> bool {
        self.show_raw_output
    }

    /// The voice channels and controls.
    pub fn voice_for_test(&self) -> &VoiceUi {
        &self.voice
    }

    /// The voice channels and controls, for a test that flips one.
    pub fn voice_mut_for_test(&mut self) -> &mut VoiceUi {
        &mut self.voice
    }

    /// The settings this GUI writes back on every control change.
    pub fn settings_mut_for_test(&mut self) -> &mut Settings {
        &mut self.settings
    }

    /// The Autopilot tab's state.
    pub fn autopilot_for_test(&self) -> &AutopilotTab {
        &self.autopilot
    }

    /// Which tab the main window shows.
    pub fn active_tab_for_test(&self) -> ActiveTab {
        self.active_tab
    }

    /// Switch tabs, the way clicking the tab bar does.
    pub fn set_active_tab_for_test(&mut self, tab: ActiveTab) {
        self.active_tab = tab;
    }

    /// The saved-conversation state.
    pub fn sessions_for_test(&self) -> &SessionState {
        &self.sessions
    }

    /// The pending image attachment slot.
    pub fn attachment_for_test(&self) -> &AttachmentSlot {
        &self.attachment
    }

    /// Attach an image to the next turn, the way a paste or a drop does.
    /// Takes both fields at once because `AttachmentSlot::set` posts its
    /// own transcript notice, so handing out the slot alone would leave a
    /// caller unable to borrow the transcript it needs.
    pub fn set_attachment_for_test(&mut self, image: ImageAttachment) {
        self.attachment.set(image, &mut self.transcript);
    }

    /// Submit the input box, the way Enter does. Wrapper: the paint path
    /// calls the real method every frame, so it keeps its own visibility.
    pub fn submit_current_input_for_test(&mut self) {
        self.submit_current_input();
    }

    /// Write `settings.json`, the way every settings-panel control does.
    /// Wrapper, for the same reason as `submit_current_input_for_test`.
    pub fn persist_settings_for_test(&self) {
        self.persist_settings();
    }

    /// Route one event, the way the frame loop does. Wrapper, for the same
    /// reason as `submit_current_input_for_test`.
    pub fn handle_routed_event_for_test(&mut self, routed: RoutedEvent) {
        self.handle_routed_event(routed);
    }

    /// Start a fresh conversation, the way the Sessions tab's button does.
    /// Wrapper, for the same reason as `submit_current_input_for_test`.
    pub fn start_new_session_for_test(&mut self) {
        self.start_new_session();
    }

    /// Open a saved conversation, the way clicking a session row does.
    /// Wrapper, for the same reason as `submit_current_input_for_test`.
    pub fn load_session_for_test(&mut self, id: SessionId) {
        self.load_session(id);
    }

    /// Delete a saved conversation, the way a row's delete control does.
    /// Wrapper, for the same reason as `submit_current_input_for_test`.
    pub fn delete_saved_session_for_test(&mut self, id: SessionId) {
        self.delete_saved_session(id);
    }

    /// Commit the working directory field, the way losing focus does.
    /// Wrapper, for the same reason as `submit_current_input_for_test`.
    pub fn commit_working_dir_change_for_test(&mut self) {
        self.commit_working_dir_change();
    }
}
