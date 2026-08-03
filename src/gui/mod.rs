use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::App;
use eframe::egui::{self, Color32, RichText, ScrollArea, TextEdit};
use egui_commonmark::CommonMarkCache;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::agent::agent_loop::StreamEvent;
use crate::agent::repeat::RepeatCommand;
use crate::config::settings::{Settings, TriggerMode};
use crate::voice::service::{VoiceCommand, VoiceEvent, VoiceState};

/// Kokoro voice ids offered by the settings panel's voice selector. A
/// fixed list, not a read of `voices/` at startup. This keeps the panel's
/// options stable and testable no matter what is unpacked on disk.
const KOKORO_VOICE_IDS: &[&str] = &[
    "af_heart",
    "af_bella",
    "af_nicole",
    "am_michael",
    "am_puck",
    "bf_emma",
    "bm_george",
];

/// Which tab the main window shows. Chat is the default; Autopilot is a
/// dedicated tab for running one task repeatedly with automatic question
/// answering, added alongside the existing settings sidebar (Tab key).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum ActiveTab {
    #[default]
    Chat,
    Autopilot,
}

/// Progress readout for the Autopilot tab. Fed from
/// `StreamEvent::RepeatIterationStart` and `StreamEvent::RepeatFinished`.
/// `Idle` before any run has started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutopilotProgress {
    Idle,
    Running { index: u32, total: u32 },
    Finished { completed: u32, total: u32 },
}

/// Native GUI using egui/eframe. Replaces the broken ratatui TUI.
pub struct DeepSeekGui {
    output_lines: Vec<(String, Color32)>,
    input_buffer: String,
    model: String,
    token_count: String,
    session_status: String,
    /// Cumulative prompt cache hit tokens (not charged as input).
    total_cache_hit_tokens: u32,
    /// Cumulative prompt cache miss tokens (charged as input).
    total_cache_miss_tokens: u32,
    rx_events: mpsc::UnboundedReceiver<StreamEvent>,
    tx_input: mpsc::UnboundedSender<String>,
    auto_scroll: bool,

    // ── Interrupt ──
    /// Flag shared with agent loop; set on Escape to abort streaming.
    interrupt_flag: Arc<AtomicBool>,

    // ── Settings panel ──
    settings_visible: bool,
    /// Sorted backend names, the keys of `settings.backends()`.
    backend_options: Vec<String>,
    selected_backend_idx: usize,

    // ── Shared state with agent ──
    thinking_flag: Arc<AtomicBool>,
    model_flag: Arc<Mutex<String>>,
    /// Flag shared with agent loop. Kept in sync with `voice_tts_enabled` so
    /// the agent knows to shape replies for speech while text to speech is on.
    voice_mode_flag: Arc<AtomicBool>,
    /// Flag shared with agent loop, holding the context budget in tokens.
    context_budget_flag: Arc<AtomicUsize>,
    /// Slider's current value, seeded from `context_budget_flag` in `new`.
    context_budget: usize,

    // ── Output display ──
    show_raw_output: bool,
    markdown_cache: CommonMarkCache,

    // ── Voice (optional). None when voice is disabled or a model is missing. ──
    voice_rx: Option<mpsc::UnboundedReceiver<VoiceEvent>>,
    voice_tx: Option<mpsc::UnboundedSender<VoiceCommand>>,
    voice_state: VoiceState,

    // ── Voice settings panel controls ──
    voice_master_enabled: bool,
    voice_stt_enabled: bool,
    voice_tts_enabled: bool,
    voice_trigger_mode: TriggerMode,
    voice_wake_phrase: String,
    voice_id_options: Vec<String>,
    selected_voice_idx: usize,
    voice_speed: f32,

    /// The model's reply text accumulated since the last `TurnEnd`, so
    /// exactly one `Speak` goes out per turn instead of one per chunk.
    /// Holds only `StreamEvent::Text` payloads, never reasoning or tool
    /// output.
    voice_reply_buffer: String,

    // ── Settings persistence ──
    /// The settings this GUI writes back on every control change. Seeded
    /// from the settings loaded at startup.
    settings: Settings,
    /// Directory holding `settings.json`, the file `persist_settings`
    /// writes.
    project_root: PathBuf,

    // ── Autopilot (optional, wired by `with_repeat`) ──
    /// Sends a repeat command to the agent task. `None` until `with_repeat`
    /// is called. The Autopilot tab built in a later step sends on this.
    repeat_tx: Option<mpsc::UnboundedSender<RepeatCommand>>,
    /// Shared with `AgentLoop::repeat_interrupt_flag`. The Autopilot tab
    /// sets this to stop a running repeat early.
    repeat_interrupt_flag: Option<Arc<AtomicBool>>,

    // ── Autopilot tab ──
    /// Which tab the main window shows. Defaults to Chat.
    active_tab: ActiveTab,
    /// Task text box in the Autopilot tab, seeded from `settings.autopilot_task()`.
    autopilot_task: String,
    /// Iteration count control in the Autopilot tab, seeded from
    /// `settings.autopilot_iterations()`.
    autopilot_iterations: u32,
    /// Progress readout state, updated by the `RepeatIterationStart` and
    /// `RepeatFinished` stream event arms.
    autopilot_progress: AutopilotProgress,
    /// Resolved policy file path, shown read-only next to the Run button.
    /// Computed once at construction from `settings.autopilot_policy_path()`
    /// and `project_root`.
    autopilot_policy_path: PathBuf,
}

impl DeepSeekGui {
    pub fn new(
        rx_events: mpsc::UnboundedReceiver<StreamEvent>,
        tx_input: mpsc::UnboundedSender<String>,
        interrupt_flag: Arc<AtomicBool>,
        thinking_flag: Arc<AtomicBool>,
        voice_mode_flag: Arc<AtomicBool>,
        context_budget_flag: Arc<AtomicUsize>,
        model_flag: Arc<Mutex<String>>,
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
        let initial_tts_enabled = settings.voice_tts_enabled();
        voice_mode_flag.store(
            voice_mode_flag_for_tts(initial_tts_enabled),
            Ordering::SeqCst,
        );
        let context_budget = context_budget_flag.load(Ordering::SeqCst);
        let voice_id_options: Vec<String> =
            KOKORO_VOICE_IDS.iter().map(|s| s.to_string()).collect();
        let configured_voice = settings.voice_tts_voice();
        let voice_idx = voice_id_options
            .iter()
            .position(|v| *v == configured_voice)
            .unwrap_or(0);
        let autopilot_task = settings.autopilot_task().unwrap_or_default();
        let autopilot_iterations = settings.autopilot_iterations();
        let autopilot_policy_path = crate::autopilot::policy::PolicyStore::new(
            project_root.clone(),
            settings.autopilot_policy_path(),
        )
        .resolved_policy_path();
        Self {
            output_lines: Vec::new(),
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
            backend_options,
            selected_backend_idx,
            thinking_flag,
            voice_mode_flag,
            context_budget_flag,
            context_budget,
            model_flag,
            show_raw_output: settings.show_raw_output(),
            markdown_cache: CommonMarkCache::default(),
            voice_rx: None,
            voice_tx: None,
            voice_state: VoiceState::Idle,
            voice_master_enabled: settings.voice_enabled(),
            voice_stt_enabled: settings.voice_stt_enabled(),
            voice_tts_enabled: initial_tts_enabled,
            voice_trigger_mode: settings.voice_trigger_mode(),
            voice_wake_phrase: settings.voice_wake_phrase(),
            voice_id_options,
            selected_voice_idx: voice_idx,
            voice_speed: settings.voice_tts_speed(),
            voice_reply_buffer: String::new(),
            settings,
            project_root,
            repeat_tx: None,
            repeat_interrupt_flag: None,
            active_tab: ActiveTab::default(),
            autopilot_task,
            autopilot_iterations,
            autopilot_progress: AutopilotProgress::Idle,
            autopilot_policy_path,
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
        self.repeat_tx = Some(repeat_tx);
        self.repeat_interrupt_flag = Some(repeat_interrupt_flag);
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
        self.voice_rx = Some(voice_rx);
        self.voice_tx = Some(voice_tx);
        self
    }

    fn handle_voice_event(&mut self, event: VoiceEvent) {
        match event {
            VoiceEvent::StateChanged(state) => {
                self.voice_state = state;
            }
            VoiceEvent::Error(message) => {
                error!(%message, "voice error event");
                self.output_lines
                    .push((format!("ERROR: {message}"), Color32::from_rgb(255, 80, 80)));
            }
            VoiceEvent::Transcript(text) => {
                // Route through the input buffer and the same submit path
                // Enter uses, so the agent sees this exactly as if it had
                // been typed. `submit_current_input` already drops empty
                // and whitespace-only text.
                self.input_buffer = text;
                self.submit_current_input();
            }
            VoiceEvent::WakeDetected => {
                // Reacting to wake detection beyond state tracking is not
                // this step's job.
            }
        }
    }

    /// Submit whatever is in `input_buffer` to the agent: append the blue
    /// "> text" line to output, mark the session running, forward the text
    /// down `tx_input`, clear the buffer. This is the single submission
    /// path both the Enter key and a voice transcript go through, so the
    /// agent cannot tell the two apart. No-ops on empty or whitespace-only
    /// input.
    fn submit_current_input(&mut self) {
        if self.input_buffer.trim().is_empty() {
            return;
        }
        let input = std::mem::take(&mut self.input_buffer);
        self.output_lines
            .push((format!("> {input}"), Color32::from_rgb(100, 149, 237)));
        self.session_status = "Running...".into();
        let _ = self.tx_input.send(input);
        self.auto_scroll = true;
    }

    /// Send a voice command if the voice subsystem is attached. Silently
    /// does nothing when voice is disabled (`voice_tx` is `None`).
    fn send_voice_command(&self, cmd: VoiceCommand) {
        if let Some(tx) = &self.voice_tx {
            let _ = tx.send(cmd);
        }
    }

    /// Speak the reply text accumulated since the last turn ended, if
    /// text to speech is on. Runs the markdown-to-speech filter first, so
    /// code fences, backticks, and URLs never reach the speaker. Always
    /// clears the buffer, spoken or not, so a later turn never inherits
    /// this one's text.
    fn speak_accumulated_reply(&mut self) {
        let reply = std::mem::take(&mut self.voice_reply_buffer);
        if !self.voice_tts_enabled {
            return;
        }
        let spoken = crate::voice::filter_for_speech(&reply);
        if spoken.is_empty() {
            return;
        }
        self.send_voice_command(VoiceCommand::Speak(spoken));
    }

    fn handle_stream_event(&mut self, event: StreamEvent) {
        match event {
            StreamEvent::Text { text, .. } => {
                self.voice_reply_buffer.push_str(&text);
                let parts: Vec<&str> = text.split('\n').collect();
                for (i, part) in parts.iter().enumerate() {
                    if i == 0 {
                        // Only append to last line if it's existing model output (white text).
                        // Don't append to user input lines, tool calls, errors, etc.
                        let can_append = self
                            .output_lines
                            .last()
                            .map(|(_, color)| *color == Color32::WHITE)
                            .unwrap_or(false);
                        if can_append {
                            if let Some((last, _)) = self.output_lines.last_mut() {
                                last.push_str(part);
                            }
                        } else {
                            self.output_lines.push((part.to_string(), Color32::WHITE));
                        }
                    } else {
                        self.output_lines.push((part.to_string(), Color32::WHITE));
                    }
                }
            }
            StreamEvent::ToolCallStart { tool, args, .. } => {
                info!(tool=%tool, args=%args, "tool call start");
                self.output_lines.push((
                    format!("\u{2699} {tool} {args}"),
                    Color32::from_rgb(255, 255, 0),
                ));
            }
            StreamEvent::ToolCallEnd {
                tool,
                output,
                is_error,
                ..
            } => {
                let color = if is_error {
                    Color32::from_rgb(255, 80, 80)
                } else {
                    Color32::from_rgb(0, 200, 0)
                };
                let preview: String = output.lines().take(10).collect::<Vec<_>>().join("\n");
                if is_error {
                    warn!(tool=%tool, error=%output, "tool call failed");
                } else {
                    debug!(tool=%tool, "tool call ok");
                }
                self.output_lines
                    .push((format!("  \u{2192} {tool}: {preview}"), color));
            }
            StreamEvent::TurnEnd {
                finish_reason,
                total_tokens,
                prompt_cache_hit_tokens,
                prompt_cache_miss_tokens,
                ..
            } => {
                self.output_lines.push((
                    format!("--- turn end ({finish_reason}) ---"),
                    Color32::from_rgb(128, 128, 128),
                ));
                self.token_count = total_tokens.to_string();
                self.total_cache_hit_tokens += prompt_cache_hit_tokens;
                self.total_cache_miss_tokens += prompt_cache_miss_tokens;
                self.speak_accumulated_reply();
            }
            StreamEvent::SessionReset => {
                self.output_lines.clear();
                self.output_lines
                    .push(("Session reset".into(), Color32::from_rgb(0, 255, 255)));
                self.session_status = "Reset".into();
                self.total_cache_hit_tokens = 0;
                self.total_cache_miss_tokens = 0;
                self.voice_reply_buffer.clear();
            }
            StreamEvent::Error { message } => {
                error!(%message, "stream error event");
                self.output_lines
                    .push((format!("ERROR: {message}"), Color32::from_rgb(255, 80, 80)));
            }
            StreamEvent::Interrupted { message } => {
                info!(%message, "agent interrupted");
                self.output_lines.push((
                    format!("\u{23F9} {message}"),
                    Color32::from_rgb(255, 165, 0),
                ));
                self.session_status = "Interrupted".into();
                // An interrupted turn never reaches TurnEnd, so nothing
                // would otherwise clear the partial reply gathered so
                // far. Drop it rather than folding it into the next
                // turn's speech.
                self.voice_reply_buffer.clear();
            }
            StreamEvent::RepeatIterationStart { index, total } => {
                info!(index, total, "repeat iteration start");
                self.autopilot_progress = AutopilotProgress::Running { index, total };
            }
            StreamEvent::RepeatFinished { completed, total } => {
                info!(completed, total, "repeat run finished");
                self.autopilot_progress = AutopilotProgress::Finished { completed, total };
            }
            StreamEvent::Reasoning { text, .. } => {
                let parts: Vec<&str> = text.split('\n').collect();
                let reason_color = Color32::from_rgb(160, 160, 160);
                for (i, part) in parts.iter().enumerate() {
                    if i == 0 {
                        let can_append = self
                            .output_lines
                            .last()
                            .map(|(_, color)| *color == reason_color)
                            .unwrap_or(false);
                        if can_append {
                            if let Some((last, _)) = self.output_lines.last_mut() {
                                last.push_str(part);
                            }
                        } else {
                            self.output_lines.push((part.to_string(), reason_color));
                        }
                    } else {
                        self.output_lines.push((part.to_string(), reason_color));
                    }
                }
            }
        }
    }

    /// Render the Chat tab's output scroll area. The heading, line count,
    /// and the markdown/raw output split are unchanged from what the
    /// central panel always rendered before the Autopilot tab existed.
    fn render_chat_output(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Output");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("Lines: {}", self.output_lines.len()))
                        .color(Color32::GRAY)
                        .small(),
                );
            });
        });
        ui.separator();
        ScrollArea::vertical()
            .stick_to_bottom(self.auto_scroll)
            .show(ui, |ui| {
                if self.show_raw_output {
                    for (text, color) in &self.output_lines {
                        ui.label(RichText::new(text).color(*color));
                    }
                } else {
                    // Group consecutive WHITE (model output) lines as markdown blocks.
                    // Non-white lines (tool calls, errors, user input, reasoning) stay raw.
                    let mut md_buf: Vec<&str> = Vec::new();
                    for (text, color) in &self.output_lines {
                        if *color == Color32::WHITE {
                            md_buf.push(text);
                        } else {
                            if !md_buf.is_empty() {
                                let md = md_buf.join("\n");
                                egui_commonmark::CommonMarkViewer::new().show(
                                    ui,
                                    &mut self.markdown_cache,
                                    &md,
                                );
                                md_buf.clear();
                            }
                            ui.label(RichText::new(text.as_str()).color(*color));
                        }
                    }
                    if !md_buf.is_empty() {
                        let md = md_buf.join("\n");
                        egui_commonmark::CommonMarkViewer::new().show(
                            ui,
                            &mut self.markdown_cache,
                            &md,
                        );
                    }
                }
            });
    }

    /// Render the Autopilot tab: task text, iteration count, the resolved
    /// policy file path, a Run button, and a progress readout.
    fn render_autopilot_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Autopilot");
        ui.separator();

        ui.label("Task");
        let task_response = ui.add(
            TextEdit::multiline(&mut self.autopilot_task)
                .desired_rows(6)
                .hint_text("Describe the task to repeat"),
        );
        // Save on focus loss, not on every keystroke, matching the wake
        // phrase field in the settings panel.
        if task_response.lost_focus() {
            let task = self.autopilot_task.clone();
            apply_autopilot_task(&mut self.settings, &task);
            self.persist_settings();
        }

        ui.add_space(8.0);

        let mut iterations = self.autopilot_iterations;
        let iter_response =
            ui.add(egui::Slider::new(&mut iterations, 1..=100).text("Iterations"));
        if iter_response.changed() {
            self.autopilot_iterations = iterations;
        }
        // Save when the drag ends, matching the other sliders in this file.
        if iter_response.drag_stopped() {
            let iterations = self.autopilot_iterations;
            apply_autopilot_iterations(&mut self.settings, iterations);
            self.persist_settings();
        }

        ui.add_space(8.0);
        ui.label(
            RichText::new(format!(
                "Policy file: {}",
                self.autopilot_policy_path.display()
            ))
            .color(Color32::GRAY)
            .small(),
        );
        ui.label(
            RichText::new(
                "Questions during a run are answered from that file by a separate model. \
                 A human never answers them.",
            )
            .color(Color32::GRAY)
            .small(),
        );

        ui.add_space(8.0);
        let can_run = !self.autopilot_task.trim().is_empty() && self.repeat_tx.is_some();
        if ui.add_enabled(can_run, egui::Button::new("Run")).clicked() {
            if let Some(tx) = &self.repeat_tx {
                let task = self.autopilot_task.clone();
                let iterations = self.autopilot_iterations;
                info!(iterations, "autopilot run requested");
                let _ = tx.send(RepeatCommand { task, iterations });
                self.autopilot_progress = AutopilotProgress::Idle;
            }
        }

        ui.add_space(8.0);
        match self.autopilot_progress {
            AutopilotProgress::Idle => {}
            AutopilotProgress::Running { index, total } => {
                ui.label(format!("Running iteration {index} of {total}"));
            }
            AutopilotProgress::Finished { completed, total } => {
                ui.label(format!("Finished: {completed} of {total} completed"));
            }
        }
    }
}

impl App for DeepSeekGui {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Poll agent events each frame
        while let Ok(event) = self.rx_events.try_recv() {
            self.handle_stream_event(event);
            self.auto_scroll = true;
        }
        // Poll voice events each frame too, alongside agent events. Drain
        // into a buffer first so the mutable borrow of `voice_rx` ends
        // before `handle_voice_event` needs `&mut self`.
        if let Some(voice_rx) = self.voice_rx.as_mut() {
            let mut voice_events = Vec::new();
            while let Ok(event) = voice_rx.try_recv() {
                voice_events.push(event);
            }
            for event in voice_events {
                self.handle_voice_event(event);
            }
        }
        // Keep polling at ~20fps even when no user input
        ctx.request_repaint_after(Duration::from_millis(50));

        // ── Settings panel (right side, Tab toggles) ──
        if self.settings_visible {
            egui::SidePanel::right("settings_panel")
                .min_width(220.0)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.heading("Settings");
                    ui.separator();

                    // ── Backend selector ──
                    let prev_idx = self.selected_backend_idx;
                    let selected_text = self
                        .backend_options
                        .get(self.selected_backend_idx)
                        .map(String::as_str)
                        .unwrap_or("(none configured)");
                    egui::ComboBox::from_label("Backend")
                        .selected_text(selected_text)
                        .show_ui(ui, |ui| {
                            for (i, opt) in self.backend_options.iter().enumerate() {
                                ui.selectable_value(&mut self.selected_backend_idx, i, opt);
                            }
                        });
                    if self.selected_backend_idx != prev_idx {
                        let new_backend = self.backend_options[self.selected_backend_idx].clone();
                        if let Some(cfg) = self.settings.resolve_backend(&new_backend) {
                            let new_model = cfg.model().to_string();
                            self.model = new_model.clone();
                            if let Ok(mut model) = self.model_flag.lock() {
                                *model = new_model;
                            }
                        }
                        info!(
                            backend = %new_backend,
                            "backend changed via settings panel"
                        );
                        apply_default_backend(&mut self.settings, &new_backend);
                        self.persist_settings();
                    }
                    ui.label(
                        RichText::new(format!("Model: {}", self.model))
                            .color(Color32::GRAY)
                            .small(),
                    );
                    ui.label(
                        RichText::new(
                            "Switching backends takes effect on the next app start.",
                        )
                        .color(Color32::GRAY)
                        .small(),
                    );

                    ui.add_space(8.0);

                    // ── Thinking toggle ──
                    let mut thinking = self.thinking_flag.load(Ordering::SeqCst);
                    if ui.checkbox(&mut thinking, "Thinking enabled").changed() {
                        self.thinking_flag.store(thinking, Ordering::SeqCst);
                        info!(thinking = thinking, "thinking toggled via settings panel");
                        apply_thinking_enabled(&mut self.settings, thinking);
                        self.persist_settings();
                    }
                    if thinking {
                        ui.label(
                            RichText::new("  Model will output reasoning trace")
                                .color(Color32::GRAY)
                                .small(),
                        );
                    }

                    ui.add_space(8.0);

                    // ── Output display toggle ──
                    let mut show_raw = self.show_raw_output;
                    if ui.checkbox(&mut show_raw, "Show raw output").changed() {
                        self.show_raw_output = show_raw;
                        apply_show_raw_output(&mut self.settings, show_raw);
                        self.persist_settings();
                    }
                    if self.show_raw_output {
                        ui.label(
                            RichText::new("  Plain text with ANSI-like coloring")
                                .color(Color32::GRAY)
                                .small(),
                        );
                    } else {
                        ui.label(
                            RichText::new("  Rendered markdown")
                                .color(Color32::GRAY)
                                .small(),
                        );
                    }

                    ui.add_space(8.0);
                    ui.separator();

                    // ── Voice section ──
                    ui.label(RichText::new("Voice").color(Color32::from_rgb(180, 220, 255)));

                    let mut voice_enabled = self.voice_master_enabled;
                    if ui.checkbox(&mut voice_enabled, "Voice enabled").changed() {
                        self.voice_master_enabled = voice_enabled;
                        self.send_voice_command(voice_enabled_command(voice_enabled));
                        info!(voice_enabled, "voice enabled toggled via settings panel");
                        apply_voice_enabled(&mut self.settings, voice_enabled);
                        self.persist_settings();
                    }

                    let mut stt_enabled = self.voice_stt_enabled;
                    if ui.checkbox(&mut stt_enabled, "Speech to text").changed() {
                        self.voice_stt_enabled = stt_enabled;
                        self.send_voice_command(stt_enabled_command(stt_enabled));
                        info!(stt_enabled, "speech-to-text toggled via settings panel");
                        apply_stt_enabled(&mut self.settings, stt_enabled);
                        self.persist_settings();
                    }

                    let mut tts_enabled = self.voice_tts_enabled;
                    if ui.checkbox(&mut tts_enabled, "Text to speech").changed() {
                        self.voice_tts_enabled = tts_enabled;
                        self.voice_mode_flag
                            .store(voice_mode_flag_for_tts(tts_enabled), Ordering::SeqCst);
                        self.send_voice_command(tts_enabled_command(tts_enabled));
                        info!(tts_enabled, "text-to-speech toggled via settings panel");
                        apply_tts_enabled(&mut self.settings, tts_enabled);
                        self.persist_settings();
                    }

                    ui.add_space(4.0);
                    ui.label(RichText::new("Trigger mode").color(Color32::GRAY).small());
                    let prev_trigger_mode = self.voice_trigger_mode;
                    ui.horizontal(|ui| {
                        ui.radio_value(
                            &mut self.voice_trigger_mode,
                            TriggerMode::PushToTalk,
                            "Push to talk",
                        );
                        ui.radio_value(
                            &mut self.voice_trigger_mode,
                            TriggerMode::WakeWord,
                            "Wake word",
                        );
                    });
                    if self.voice_trigger_mode != prev_trigger_mode {
                        self.send_voice_command(trigger_mode_command(self.voice_trigger_mode));
                        info!(
                            mode = ?self.voice_trigger_mode,
                            "voice trigger mode changed via settings panel"
                        );
                        let mode = self.voice_trigger_mode;
                        apply_trigger_mode(&mut self.settings, mode);
                        self.persist_settings();
                    }

                    ui.add_space(4.0);
                    let wake_response = ui.add(
                        TextEdit::singleline(&mut self.voice_wake_phrase).hint_text("wake phrase"),
                    );
                    if wake_response.changed() {
                        self.send_voice_command(wake_phrase_command(&self.voice_wake_phrase));
                        info!(
                            phrase = %self.voice_wake_phrase,
                            "wake phrase changed via settings panel"
                        );
                    }
                    // Save on focus loss, not on every keystroke, so typing
                    // a phrase writes the file once.
                    if wake_response.lost_focus() {
                        let phrase = self.voice_wake_phrase.clone();
                        apply_wake_phrase(&mut self.settings, &phrase);
                        self.persist_settings();
                    }

                    ui.add_space(4.0);
                    let prev_voice_idx = self.selected_voice_idx;
                    egui::ComboBox::from_label("Kokoro voice")
                        .selected_text(&self.voice_id_options[self.selected_voice_idx])
                        .show_ui(ui, |ui| {
                            for (i, opt) in self.voice_id_options.iter().enumerate() {
                                ui.selectable_value(&mut self.selected_voice_idx, i, opt);
                            }
                        });
                    if self.selected_voice_idx != prev_voice_idx {
                        let voice_id = self.voice_id_options[self.selected_voice_idx].clone();
                        self.send_voice_command(voice_id_command(&voice_id));
                        info!(voice_id = %voice_id, "kokoro voice changed via settings panel");
                        apply_tts_voice(&mut self.settings, &voice_id);
                        self.persist_settings();
                    }

                    ui.add_space(4.0);
                    let speed_response =
                        ui.add(egui::Slider::new(&mut self.voice_speed, 0.5..=2.0).text("Speed"));
                    if speed_response.changed() {
                        self.send_voice_command(speed_command(self.voice_speed));
                        info!(
                            speed = self.voice_speed,
                            "voice speed changed via settings panel"
                        );
                    }
                    // Save when the drag ends, so one drag writes the file
                    // once instead of once per frame.
                    if speed_response.drag_stopped() {
                        let speed = self.voice_speed;
                        apply_tts_speed(&mut self.settings, speed);
                        self.persist_settings();
                    }

                    ui.add_space(8.0);
                    ui.separator();

                    // ── Experimental features section ──
                    ui.label(RichText::new("Experimental").color(Color32::from_rgb(255, 200, 100)));

                    let mut context_budget = self.context_budget;
                    let budget_response = ui.add(
                        egui::Slider::new(&mut context_budget, 32_000..=200_000)
                            .step_by(1000.0)
                            .text("Context budget"),
                    );
                    if budget_response.changed() {
                        self.context_budget = context_budget;
                        self.context_budget_flag
                            .store(context_budget, Ordering::SeqCst);
                        info!(
                            context_budget = context_budget,
                            "context budget changed via settings panel"
                        );
                    }
                    // Same as the speed slider: one write per drag.
                    if budget_response.drag_stopped() {
                        let budget = self.context_budget;
                        apply_context_budget(&mut self.settings, budget);
                        self.persist_settings();
                    }
                    ui.label(
                        RichText::new(format!(
                            "  Prunes to {} tokens when exceeded",
                            self.context_budget / 3
                        ))
                        .color(Color32::GRAY)
                        .small(),
                    );

                    ui.add_space(16.0);
                    ui.separator();

                    // ── Close button ──
                    if ui.button("Close panel (Tab)").clicked() {
                        self.settings_visible = false;
                    }

                    ui.add_space(4.0);
                    if ui.button("Quit (Ctrl+Q)").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
        }

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

        // Space held -> push-to-talk, only while not typing and the
        // settings panel is closed.
        if let Some(signal) = space_ptt_signal(
            space_pressed,
            space_released,
            ctrl_held,
            any_widget_focused,
            self.settings_visible,
        ) {
            self.send_voice_command(match signal {
                PttSignal::Start => VoiceCommand::StartListening,
                PttSignal::Stop => VoiceCommand::StopListening,
            });
        }

        // Ctrl+Space -> push-to-talk toggle, works even while typing,
        // still closed off by the settings panel.
        if let Some(signal) = ctrl_space_toggle_signal(
            ctrl_space_pressed,
            self.settings_visible,
            self.voice_state == VoiceState::Listening,
        ) {
            self.send_voice_command(match signal {
                PttSignal::Start => VoiceCommand::StartListening,
                PttSignal::Stop => VoiceCommand::StopListening,
            });
        }

        // Escape - interrupt agent, stop any speech in progress, and stop
        // a running autopilot repeat. One Escape cuts off whatever the
        // session is doing, in any tab.
        if escape {
            info!("user pressed Escape - interrupting agent");
            self.interrupt_flag.store(true, Ordering::SeqCst);
            if let Some(flag) = &self.repeat_interrupt_flag {
                flag.store(true, Ordering::SeqCst);
            }
            self.send_voice_command(VoiceCommand::StopSpeaking);
            self.output_lines
                .push(("[Interrupting...]".into(), Color32::from_rgb(255, 165, 0)));
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
            });
            ui.separator();
            match self.active_tab {
                ActiveTab::Chat => self.render_chat_output(ui),
                ActiveTab::Autopilot => self.render_autopilot_tab(ui),
            }
        });
        self.auto_scroll = false;

        // ── Input bar (Chat tab only) ──
        if self.active_tab == ActiveTab::Chat {
            egui::TopBottomPanel::bottom("input_panel")
                .min_height(32.0)
                .show(ctx, |ui| {
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
                    let thinking_label = if self.thinking_flag.load(Ordering::SeqCst) {
                        "Think: ON"
                    } else {
                        "Think: OFF"
                    };
                    ui.label(
                        RichText::new(thinking_label)
                            .color(Color32::from_rgb(200, 200, 100))
                            .small(),
                    );
                    ui.separator();
                    ui.label(
                        RichText::new(voice_state_label(self.voice_state))
                            .color(voice_state_color(self.voice_state))
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

/// Which push-to-talk action a key event should produce, if any. Kept
/// separate from `VoiceCommand` so the decision logic below can be unit
/// tested without constructing voice commands or an egui context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PttSignal {
    Start,
    Stop,
}

/// Decide the push-to-talk action for the space-bar-held binding. Space
/// types a space character when the text input has focus, so this binding
/// only ever fires when the input does NOT have focus. Also suppressed
/// while Ctrl is held (that is the separate Ctrl+Space toggle binding) or
/// the settings panel is open. `Start` fires on press, `Stop` fires on
/// release, matching "held" rather than "toggled".
fn space_ptt_signal(
    space_pressed: bool,
    space_released: bool,
    ctrl_held: bool,
    input_focused: bool,
    settings_visible: bool,
) -> Option<PttSignal> {
    if ctrl_held || input_focused || settings_visible {
        return None;
    }
    if space_pressed {
        Some(PttSignal::Start)
    } else if space_released {
        Some(PttSignal::Stop)
    } else {
        None
    }
}

/// Decide the push-to-talk action for the Ctrl+Space toggle binding.
/// Unlike the space-bar-held binding, this works even when the text input
/// has focus. Ctrl+Space is not a printable character, so it never
/// collides with typing. Still suppressed while the settings panel is
/// open. Toggles off `currently_listening` rather than press/release.
/// This keeps the state coherent: a listening session this binding
/// started is always one more press of the same key away from stopping.
fn ctrl_space_toggle_signal(
    ctrl_space_pressed: bool,
    settings_visible: bool,
    currently_listening: bool,
) -> Option<PttSignal> {
    if !ctrl_space_pressed || settings_visible {
        return None;
    }
    Some(if currently_listening {
        PttSignal::Stop
    } else {
        PttSignal::Start
    })
}

/// Short status-bar label for a voice state.
fn voice_state_label(state: VoiceState) -> &'static str {
    match state {
        VoiceState::Idle => "Idle",
        VoiceState::Listening => "Listening",
        VoiceState::Transcribing => "Transcribing",
        VoiceState::Speaking => "Speaking",
    }
}

/// Status-bar color for a voice state. See [`voice_state_label`].
fn voice_state_color(state: VoiceState) -> Color32 {
    match state {
        VoiceState::Idle => Color32::from_rgb(128, 128, 128),
        VoiceState::Listening => Color32::from_rgb(0, 200, 0),
        VoiceState::Transcribing => Color32::from_rgb(255, 255, 0),
        VoiceState::Speaking => Color32::from_rgb(100, 149, 237),
    }
}

// ── Voice control-to-command mapping ──
//
// Each function below takes the plain value a settings-panel control just
// changed to and returns the `VoiceCommand` that change should send. Kept
// separate from the control's egui code so each mapping is unit-testable
// without an egui context, the same way `space_ptt_signal` and
// `ctrl_space_toggle_signal` above are.

/// Build the command for the master voice-enable checkbox.
fn voice_enabled_command(enabled: bool) -> VoiceCommand {
    VoiceCommand::SetEnabled(enabled)
}

/// Build the command for the speech-to-text checkbox.
fn stt_enabled_command(enabled: bool) -> VoiceCommand {
    VoiceCommand::SetSttEnabled(enabled)
}

/// Build the command for the text-to-speech checkbox.
fn tts_enabled_command(enabled: bool) -> VoiceCommand {
    VoiceCommand::SetTtsEnabled(enabled)
}

/// Report the value `voice_mode_flag` should hold for a given text-to-speech
/// checkbox state. The flag always matches the checkbox.
fn voice_mode_flag_for_tts(tts_enabled: bool) -> bool {
    tts_enabled
}

/// Build the command for the trigger-mode radio pair.
fn trigger_mode_command(mode: TriggerMode) -> VoiceCommand {
    VoiceCommand::SetTriggerMode(mode)
}

/// Build the command for the wake-phrase text field.
fn wake_phrase_command(phrase: &str) -> VoiceCommand {
    VoiceCommand::SetWakePhrase(phrase.to_string())
}

/// Build the command for the Kokoro voice id selector.
fn voice_id_command(voice_id: &str) -> VoiceCommand {
    VoiceCommand::SetVoice(voice_id.to_string())
}

/// Build the command for the speech speed slider.
fn speed_command(speed: f32) -> VoiceCommand {
    VoiceCommand::SetSpeed(speed)
}

// ── Control-to-settings mapping ──
//
// Each function below takes the plain value a settings-panel control just
// changed to and writes it into a `Settings`. Kept separate from the
// control's egui code so each write is unit-testable without an egui
// context, the same way the command builders above are. The voice and
// thinking writers create their config block when it is missing, so a
// change is never silently dropped.

/// Store the backend picker's selection.
fn apply_default_backend(settings: &mut Settings, name: &str) {
    settings.default_backend = Some(name.to_string());
}

/// Store the thinking checkbox's value.
fn apply_thinking_enabled(settings: &mut Settings, enabled: bool) {
    settings.thinking_mut().enabled = enabled;
}

/// Store the raw-output checkbox's value.
fn apply_show_raw_output(settings: &mut Settings, show_raw: bool) {
    settings.show_raw_output = Some(show_raw);
}

/// Store the master voice checkbox's value.
fn apply_voice_enabled(settings: &mut Settings, enabled: bool) {
    settings.voice_mut().enabled = enabled;
}

/// Store the speech-to-text checkbox's value.
fn apply_stt_enabled(settings: &mut Settings, enabled: bool) {
    settings.voice_mut().stt_enabled = enabled;
}

/// Store the text-to-speech checkbox's value.
fn apply_tts_enabled(settings: &mut Settings, enabled: bool) {
    settings.voice_mut().tts_enabled = enabled;
}

/// Store the trigger-mode radio pair's selection.
fn apply_trigger_mode(settings: &mut Settings, mode: TriggerMode) {
    settings.voice_mut().trigger_mode = mode;
}

/// Store the wake-phrase field's text.
fn apply_wake_phrase(settings: &mut Settings, phrase: &str) {
    settings.voice_mut().wake_phrase = Some(phrase.to_string());
}

/// Store the Kokoro voice selector's choice.
fn apply_tts_voice(settings: &mut Settings, voice_id: &str) {
    settings.voice_mut().tts_voice = Some(voice_id.to_string());
}

/// Store the speech speed slider's value.
fn apply_tts_speed(settings: &mut Settings, speed: f32) {
    settings.voice_mut().tts_speed = Some(speed);
}

/// Store the context budget slider's value.
fn apply_context_budget(settings: &mut Settings, budget: usize) {
    settings.context_budget = Some(budget);
}

/// Store the Autopilot tab's task text box.
fn apply_autopilot_task(settings: &mut Settings, task: &str) {
    settings.autopilot_mut().task = Some(task.to_string());
}

/// Store the Autopilot tab's iteration count control.
fn apply_autopilot_iterations(settings: &mut Settings, iterations: u32) {
    settings.autopilot_mut().iterations = Some(iterations);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::settings::{ApiProvider, BackendConfig, VoiceConfig};
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
        let dir = std::env::temp_dir().join(format!("dsc-gui-{tag}-{}-{nanos}", std::process::id()));
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
        DeepSeekGui::new(
            rx_events,
            tx_input,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicUsize::new(100_000)),
            Arc::new(Mutex::new("deepseek-v4-flash".into())),
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
            },
        );
        backends.insert(
            "beta".to_string(),
            BackendConfig::ClaudeCli {
                model: "beta-model".to_string(),
                permission_mode: None,
                env: None,
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
    fn reasoning_event_adds_payload_line() {
        let mut gui = make_gui();
        gui.handle_stream_event(StreamEvent::Reasoning {
            turn: 1,
            text: "Let me think about this...".into(),
        });
        assert_eq!(gui.output_lines.len(), 1);
        assert_eq!(gui.output_lines[0].0, "Let me think about this...");
        assert_eq!(gui.output_lines[0].1, Color32::from_rgb(160, 160, 160));
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
        assert_eq!(gui.output_lines.len(), 1);
        assert_eq!(gui.output_lines[0].0, "FirstSecond");
    }

    #[test]
    fn text_event_creates_white_payload_lines() {
        let mut gui = make_gui();
        gui.handle_stream_event(StreamEvent::Text {
            turn: 1,
            text: "Hello world".into(),
        });
        assert_eq!(gui.output_lines.len(), 1);
        assert_eq!(gui.output_lines[0].0, "Hello world");
        assert_eq!(gui.output_lines[0].1, Color32::WHITE);
    }

    #[test]
    fn user_input_line_is_blue_not_white() {
        let mut gui = make_gui();
        // Simulate what happens when user presses Enter:
        gui.output_lines.push((
            "> pick a number between 1 and 100".into(),
            Color32::from_rgb(100, 149, 237),
        ));
        let (text, color) = &gui.output_lines[0];
        assert_ne!(
            *color,
            Color32::WHITE,
            "user input must not be white (would render as markdown)"
        );
        assert_eq!(*color, Color32::from_rgb(100, 149, 237));
        assert!(
            text.starts_with("> "),
            "user input starts with > which would be blockquote in markdown"
        );
    }

    #[test]
    fn model_output_is_white_for_markdown_rendering() {
        let mut gui = make_gui();
        gui.handle_stream_event(StreamEvent::Text {
            turn: 1,
            text: "**bold** and *italic*".into(),
        });
        let (_, color) = &gui.output_lines[0];
        assert_eq!(
            *color,
            Color32::WHITE,
            "model output must be white to trigger markdown rendering"
        );
    }

    /// Simulates a full thinking-enabled interaction: user input → reasoning → text → turn end.
    /// Proves: user input is blue (won't be markdown), reasoning is grey, model output is white.
    #[test]
    fn full_thinking_interaction_produces_correct_colors() {
        let mut gui = make_gui();

        // 1. Simulate user pressing Enter with "> " prefixed input (as the GUI does at line 364-365)
        gui.output_lines.push((
            "> pick a number between 1 and 100 but don't tell me".into(),
            Color32::from_rgb(100, 149, 237), // blue — matches gui code
        ));

        // 2. Reasoning chunk arrives from agent (thinking enabled)
        gui.handle_stream_event(StreamEvent::Reasoning {
            turn: 1,
            text: "The user wants me to pick a secret number.".into(),
        });

        // 3. More reasoning (appends to same line)
        gui.handle_stream_event(StreamEvent::Reasoning {
            turn: 1,
            text: " I'll pick 42.".into(),
        });

        // 4. Model text response (markdown)
        gui.handle_stream_event(StreamEvent::Text {
            turn: 1,
            text: "I've picked a number between 1 and 100.".into(),
        });

        // 5. Turn end
        gui.handle_stream_event(StreamEvent::TurnEnd {
            turn: 1,
            finish_reason: "stop".into(),
            total_tokens: 150,
            prompt_cache_hit_tokens: 0,
            prompt_cache_miss_tokens: 0,
        });

        assert_eq!(
            gui.output_lines.len(),
            4,
            "expected 4 lines: user input, reasoning, text, turn end"
        );

        // Line 0: user input — must NOT be white (would become markdown)
        let (text0, color0) = &gui.output_lines[0];
        assert_ne!(
            *color0,
            Color32::WHITE,
            "user input must not be white (markdown)"
        );
        assert_eq!(
            *color0,
            Color32::from_rgb(100, 149, 237),
            "user input must be blue"
        );
        assert!(
            text0.starts_with("> "),
            "user input has > prefix that would be blockquote in markdown"
        );

        // Line 1: reasoning — grey, visible in output
        let (text1, color1) = &gui.output_lines[1];
        assert_eq!(
            *color1,
            Color32::from_rgb(160, 160, 160),
            "reasoning must be grey"
        );
        assert_eq!(
            text1,
            "The user wants me to pick a secret number. I'll pick 42."
        );

        // Line 2: model output — white, will be rendered as markdown
        let (text2, color2) = &gui.output_lines[2];
        assert_eq!(
            *color2,
            Color32::WHITE,
            "model output must be white for markdown rendering"
        );
        assert_eq!(text2, "I've picked a number between 1 and 100.");

        // Line 3: turn end — grey status
        let (text3, color3) = &gui.output_lines[3];
        assert_eq!(
            *color3,
            Color32::from_rgb(128, 128, 128),
            "turn end must be grey"
        );
        assert!(
            text3.contains("turn end"),
            "turn end line must contain 'turn end'"
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
    fn new_gui_has_no_voice_channels_and_starts_idle() {
        let gui = make_gui();
        assert!(gui.voice_rx.is_none());
        assert!(gui.voice_tx.is_none());
        assert_eq!(gui.voice_state, VoiceState::Idle);
    }

    #[test]
    fn with_voice_attaches_both_channels() {
        let gui = make_gui();
        let (_tx_voice_events, rx_voice) = mpsc::unbounded_channel::<VoiceEvent>();
        let (tx_voice_cmd, _rx_voice_cmd) = mpsc::unbounded_channel::<VoiceCommand>();
        let gui = gui.with_voice(rx_voice, tx_voice_cmd);
        assert!(gui.voice_rx.is_some());
        assert!(gui.voice_tx.is_some());
    }

    #[test]
    fn new_seeds_every_panel_control_from_settings() {
        let gui = make_gui_with_settings(&settings_with_voice("am_michael"));
        assert!(gui.show_raw_output);
        assert!(gui.voice_master_enabled);
        assert!(gui.voice_stt_enabled);
        assert!(gui.voice_tts_enabled);
        assert_eq!(gui.voice_trigger_mode, TriggerMode::WakeWord);
        assert_eq!(gui.voice_wake_phrase, "hey computer");
        assert_eq!(gui.voice_id_options[gui.selected_voice_idx], "am_michael");
        assert_eq!(gui.voice_speed, 1.4);
    }

    #[test]
    fn new_falls_back_to_the_first_voice_on_an_unknown_voice_id() {
        let gui = make_gui_with_settings(&settings_with_voice("zz_nobody"));
        assert_eq!(gui.selected_voice_idx, 0);
        assert_eq!(gui.voice_id_options[0], "af_heart");
    }

    #[test]
    fn new_sets_voice_mode_flag_from_the_seeded_tts_value() {
        let gui = make_gui_with_settings(&settings_with_voice("af_heart"));
        assert!(gui.voice_tts_enabled);
        assert!(gui.voice_mode_flag.load(Ordering::SeqCst));

        let gui = make_gui();
        assert!(!gui.voice_tts_enabled);
        assert!(!gui.voice_mode_flag.load(Ordering::SeqCst));
    }

    #[test]
    fn voice_state_changed_event_updates_tracked_state() {
        let mut gui = make_gui();
        gui.handle_voice_event(VoiceEvent::StateChanged(VoiceState::Listening));
        assert_eq!(gui.voice_state, VoiceState::Listening);
    }

    #[test]
    fn voice_error_event_adds_output_line_in_error_style() {
        let mut gui = make_gui();
        gui.handle_voice_event(VoiceEvent::Error("mic unavailable".into()));
        assert_eq!(gui.output_lines.len(), 1);
        assert_eq!(gui.output_lines[0].0, "ERROR: mic unavailable");
        assert_eq!(gui.output_lines[0].1, Color32::from_rgb(255, 80, 80));
    }

    #[test]
    fn wake_detected_event_adds_no_output_line() {
        let mut gui = make_gui();
        gui.handle_voice_event(VoiceEvent::WakeDetected);
        assert!(gui.output_lines.is_empty());
        assert_eq!(gui.voice_state, VoiceState::Idle);
    }

    #[test]
    fn transcript_event_submits_through_the_enter_path() {
        let mut gui = make_gui();
        gui.handle_voice_event(VoiceEvent::Transcript("turn on the lights".into()));
        assert_eq!(gui.output_lines.len(), 1);
        assert_eq!(gui.output_lines[0].0, "> turn on the lights");
        assert_eq!(gui.output_lines[0].1, Color32::from_rgb(100, 149, 237));
        assert!(
            gui.input_buffer.is_empty(),
            "buffer clears on submit, same as Enter"
        );
        assert_eq!(gui.session_status, "Running...");
    }

    #[test]
    fn transcript_event_forwards_text_to_the_agent_channel() {
        let (tx_events, rx_events) = mpsc::unbounded_channel();
        let (tx_input, mut rx_input) = mpsc::unbounded_channel();
        let mut gui = DeepSeekGui::new(
            rx_events,
            tx_input,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicUsize::new(100_000)),
            Arc::new(Mutex::new("deepseek-v4-flash".into())),
            Settings::default(),
            unique_temp_dir("ctor"),
        );
        gui.handle_voice_event(VoiceEvent::Transcript("hello".into()));
        assert_eq!(rx_input.try_recv().unwrap(), "hello");
    }

    #[test]
    fn empty_transcript_is_not_submitted() {
        let mut gui = make_gui();
        gui.handle_voice_event(VoiceEvent::Transcript("".into()));
        assert!(gui.output_lines.is_empty());
        assert_eq!(gui.session_status, "Ready");
    }

    #[test]
    fn whitespace_only_transcript_is_not_submitted() {
        let mut gui = make_gui();
        gui.handle_voice_event(VoiceEvent::Transcript("   ".into()));
        assert!(gui.output_lines.is_empty());
        assert_eq!(gui.session_status, "Ready");
    }

    #[test]
    fn space_ptt_starts_listening_on_press_when_input_not_focused() {
        let signal = space_ptt_signal(true, false, false, false, false);
        assert_eq!(signal, Some(PttSignal::Start));
    }

    #[test]
    fn space_ptt_stops_listening_on_release() {
        let signal = space_ptt_signal(false, true, false, false, false);
        assert_eq!(signal, Some(PttSignal::Stop));
    }

    #[test]
    fn space_does_not_trigger_listening_while_input_is_focused() {
        let signal = space_ptt_signal(true, false, false, true, false);
        assert_eq!(
            signal, None,
            "space must type a space, not start listening, while the user is typing"
        );
    }

    #[test]
    fn space_ptt_suppressed_while_settings_panel_is_open() {
        let signal = space_ptt_signal(true, false, false, false, true);
        assert_eq!(signal, None);
    }

    #[test]
    fn space_ptt_suppressed_while_ctrl_is_held() {
        let signal = space_ptt_signal(true, false, true, false, false);
        assert_eq!(
            signal, None,
            "ctrl+space is the separate toggle binding, not the held binding"
        );
    }

    #[test]
    fn ctrl_space_toggle_starts_listening_when_idle() {
        let signal = ctrl_space_toggle_signal(true, false, false);
        assert_eq!(signal, Some(PttSignal::Start));
    }

    #[test]
    fn ctrl_space_toggle_stops_listening_when_already_listening() {
        let signal = ctrl_space_toggle_signal(true, false, true);
        assert_eq!(signal, Some(PttSignal::Stop));
    }

    #[test]
    fn ctrl_space_toggle_takes_no_input_focused_parameter_and_always_fires() {
        // This binding must work even while the text input has focus, so
        // its signature has no focus parameter to gate on in the first
        // place. This test only confirms it fires given ctrl+space pressed.
        let signal = ctrl_space_toggle_signal(true, false, false);
        assert_eq!(signal, Some(PttSignal::Start));
    }

    #[test]
    fn ctrl_space_toggle_suppressed_while_settings_panel_is_open() {
        let signal = ctrl_space_toggle_signal(true, true, false);
        assert_eq!(signal, None);
    }

    #[test]
    fn ctrl_space_toggle_does_nothing_when_key_not_pressed() {
        let signal = ctrl_space_toggle_signal(false, false, false);
        assert_eq!(signal, None);
    }

    #[test]
    fn voice_state_label_text_for_each_state() {
        assert_eq!(voice_state_label(VoiceState::Idle), "Idle");
        assert_eq!(voice_state_label(VoiceState::Listening), "Listening");
        assert_eq!(voice_state_label(VoiceState::Transcribing), "Transcribing");
        assert_eq!(voice_state_label(VoiceState::Speaking), "Speaking");
    }

    #[test]
    fn voice_state_color_for_each_state() {
        assert_eq!(
            voice_state_color(VoiceState::Idle),
            Color32::from_rgb(128, 128, 128),
            "idle must be grey"
        );
        assert_eq!(
            voice_state_color(VoiceState::Listening),
            Color32::from_rgb(0, 200, 0),
            "listening must be green"
        );
        assert_eq!(
            voice_state_color(VoiceState::Transcribing),
            Color32::from_rgb(255, 255, 0),
            "transcribing must be yellow"
        );
        assert_eq!(
            voice_state_color(VoiceState::Speaking),
            Color32::from_rgb(100, 149, 237),
            "speaking must be blue"
        );
    }

    #[test]
    fn voice_enabled_command_wraps_the_checkbox_value() {
        assert_eq!(voice_enabled_command(true), VoiceCommand::SetEnabled(true));
        assert_eq!(
            voice_enabled_command(false),
            VoiceCommand::SetEnabled(false)
        );
    }

    #[test]
    fn stt_enabled_command_wraps_the_checkbox_value() {
        assert_eq!(stt_enabled_command(true), VoiceCommand::SetSttEnabled(true));
        assert_eq!(
            stt_enabled_command(false),
            VoiceCommand::SetSttEnabled(false)
        );
    }

    #[test]
    fn tts_enabled_command_wraps_the_checkbox_value() {
        assert_eq!(tts_enabled_command(true), VoiceCommand::SetTtsEnabled(true));
        assert_eq!(
            tts_enabled_command(false),
            VoiceCommand::SetTtsEnabled(false)
        );
    }

    #[test]
    fn voice_mode_flag_for_tts_matches_the_checkbox_state() {
        assert!(voice_mode_flag_for_tts(true));
        assert!(!voice_mode_flag_for_tts(false));
    }

    #[test]
    fn voice_mode_flag_starts_matching_initial_tts_state() {
        let gui = make_gui();
        assert!(!gui.voice_tts_enabled);
        assert!(!gui.voice_mode_flag.load(Ordering::SeqCst));
    }

    #[test]
    fn voice_mode_flag_tracks_tts_toggling_on_then_off() {
        let mut gui = make_gui();

        gui.voice_tts_enabled = true;
        gui.voice_mode_flag.store(
            voice_mode_flag_for_tts(gui.voice_tts_enabled),
            Ordering::SeqCst,
        );
        assert!(gui.voice_mode_flag.load(Ordering::SeqCst));

        gui.voice_tts_enabled = false;
        gui.voice_mode_flag.store(
            voice_mode_flag_for_tts(gui.voice_tts_enabled),
            Ordering::SeqCst,
        );
        assert!(!gui.voice_mode_flag.load(Ordering::SeqCst));
    }

    #[test]
    fn trigger_mode_command_wraps_the_selected_mode() {
        assert_eq!(
            trigger_mode_command(TriggerMode::PushToTalk),
            VoiceCommand::SetTriggerMode(TriggerMode::PushToTalk)
        );
        assert_eq!(
            trigger_mode_command(TriggerMode::WakeWord),
            VoiceCommand::SetTriggerMode(TriggerMode::WakeWord)
        );
    }

    #[test]
    fn wake_phrase_command_wraps_the_field_text() {
        assert_eq!(
            wake_phrase_command("hey computer"),
            VoiceCommand::SetWakePhrase("hey computer".to_string())
        );
    }

    #[test]
    fn voice_id_command_wraps_the_selected_voice() {
        assert_eq!(
            voice_id_command("am_michael"),
            VoiceCommand::SetVoice("am_michael".to_string())
        );
    }

    #[test]
    fn speed_command_wraps_the_slider_value() {
        assert_eq!(speed_command(1.5), VoiceCommand::SetSpeed(1.5));
    }

    #[test]
    fn new_gui_has_sensible_voice_control_defaults() {
        let gui = make_gui();
        assert!(!gui.voice_master_enabled);
        assert!(!gui.voice_stt_enabled);
        assert!(!gui.voice_tts_enabled);
        assert_eq!(gui.voice_trigger_mode, TriggerMode::PushToTalk);
        assert_eq!(gui.voice_wake_phrase, "hey deepseek");
        assert_eq!(gui.selected_voice_idx, 0);
        assert_eq!(gui.voice_speed, 1.0);
        assert_eq!(gui.voice_id_options.len(), KOKORO_VOICE_IDS.len());
        assert_eq!(gui.voice_id_options[0], "af_heart");
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
        gui.voice_tts_enabled = true;

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
        assert!(gui.voice_reply_buffer.is_empty());
    }

    #[test]
    fn turn_end_sends_nothing_when_tts_is_disabled() {
        let (mut gui, mut rx_voice_cmd) = make_gui_with_voice();
        assert!(!gui.voice_tts_enabled, "tts is off by default");

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
        gui.voice_tts_enabled = true;

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
        gui.voice_tts_enabled = true;

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
        gui.voice_tts_enabled = true;

        gui.handle_stream_event(StreamEvent::Text {
            turn: 1,
            text: "partial reply".into(),
        });
        gui.handle_stream_event(StreamEvent::Interrupted {
            message: "Interrupted by user (Escape)".into(),
        });
        assert!(gui.voice_reply_buffer.is_empty());

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
        let (tx_events, rx_events) = mpsc::unbounded_channel();
        let (tx_input, _rx_input) = mpsc::unbounded_channel();
        let gui = DeepSeekGui::new(
            rx_events,
            tx_input,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicUsize::new(64_000)),
            Arc::new(Mutex::new("deepseek-v4-flash".into())),
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
    fn thinking_change_creates_the_block_when_it_is_missing() {
        let mut settings = Settings::default();
        assert!(settings.thinking.is_none(), "no thinking block to start");
        apply_thinking_enabled(&mut settings, true);
        assert!(settings.thinking.is_some(), "the block gets created");

        let loaded = round_trip(|s| apply_thinking_enabled(s, true));
        assert!(loaded.thinking_enabled());
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
        assert_eq!(gui.autopilot_progress, AutopilotProgress::Idle);

        gui.handle_stream_event(StreamEvent::RepeatIterationStart { index: 2, total: 5 });
        assert_eq!(
            gui.autopilot_progress,
            AutopilotProgress::Running { index: 2, total: 5 }
        );

        gui.handle_stream_event(StreamEvent::RepeatFinished {
            completed: 5,
            total: 5,
        });
        assert_eq!(
            gui.autopilot_progress,
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
        let store =
            crate::autopilot::policy::PolicyStore::new(root.clone(), Some("custom-policy.md".into()));
        let path = store.resolved_policy_path();
        assert_eq!(path, root.join("custom-policy.md"));
    }

    #[test]
    fn new_seeds_autopilot_controls_from_settings() {
        let mut settings = Settings::default();
        settings.autopilot_mut().task = Some("run the tests".to_string());
        settings.autopilot_mut().iterations = Some(9);
        let gui = make_gui_with_settings(&settings);
        assert_eq!(gui.autopilot_task, "run the tests");
        assert_eq!(gui.autopilot_iterations, 9);
    }

    #[test]
    fn session_reset_clears_the_pending_reply() {
        let mut gui = make_gui();
        gui.handle_stream_event(StreamEvent::Text {
            turn: 1,
            text: "partial reply".into(),
        });
        gui.handle_stream_event(StreamEvent::SessionReset);
        assert!(gui.voice_reply_buffer.is_empty());
    }
}
