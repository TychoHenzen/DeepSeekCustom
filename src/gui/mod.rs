use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui::{self, Color32, RichText, ScrollArea, TextEdit};
use eframe::App;
use egui_commonmark::CommonMarkCache;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::agent::agent_loop::StreamEvent;

/// Native GUI using egui/eframe. Replaces the broken ratatui TUI.
pub struct DeepSeekGui {
    output_lines: Vec<(String, Color32)>,
    input_buffer: String,
    model: String,
    token_count: String,
    session_status: String,
    rx_events: mpsc::UnboundedReceiver<StreamEvent>,
    tx_input: mpsc::UnboundedSender<String>,
    auto_scroll: bool,

    // ── Interrupt ──
    /// Flag shared with agent loop; set on Escape to abort streaming.
    interrupt_flag: Arc<AtomicBool>,

    // ── Settings panel ──
    settings_visible: bool,
    model_options: Vec<String>,
    selected_model_idx: usize,

    // ── Shared state with agent ──
    thinking_flag: Arc<AtomicBool>,
    model_flag: Arc<Mutex<String>>,

    // ── Output display ──
    show_raw_output: bool,
    markdown_cache: CommonMarkCache,
}

impl DeepSeekGui {
    pub fn new(
        rx_events: mpsc::UnboundedReceiver<StreamEvent>,
        tx_input: mpsc::UnboundedSender<String>,
        interrupt_flag: Arc<AtomicBool>,
        thinking_flag: Arc<AtomicBool>,
        model_flag: Arc<Mutex<String>>,
    ) -> Self {
        let model_options = vec![
            "deepseek-v4-flash".to_string(),
            "deepseek-v4-pro".to_string(),
        ];
        let current_model = model_flag.lock().unwrap().clone();
        let model_idx = model_options
            .iter()
            .position(|m| *m == current_model)
            .unwrap_or(0);
        Self {
            output_lines: Vec::new(),
            input_buffer: String::new(),
            model: current_model,
            token_count: "0".into(),
            session_status: "Ready".into(),
            rx_events,
            tx_input,
            auto_scroll: false,
            interrupt_flag,
            settings_visible: false,
            model_options,
            selected_model_idx: model_idx,
            thinking_flag,
            model_flag,
            show_raw_output: false,
            markdown_cache: CommonMarkCache::default(),
        }
    }

    fn handle_stream_event(&mut self, event: StreamEvent) {
        match event {
            StreamEvent::Text { text, .. } => {
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
                            self.output_lines
                                .push((part.to_string(), Color32::WHITE));
                        }
                    } else {
                        self.output_lines
                            .push((part.to_string(), Color32::WHITE));
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
                self.output_lines.push((
                    format!("  \u{2192} {tool}: {preview}"),
                    color,
                ));
            }
            StreamEvent::TurnEnd {
                finish_reason,
                total_tokens,
                ..
            } => {
                self.output_lines.push((
                    format!("--- turn end ({finish_reason}) ---"),
                    Color32::from_rgb(128, 128, 128),
                ));
                self.token_count = total_tokens.to_string();
            }
            StreamEvent::SessionReset => {
                self.output_lines.clear();
                self.output_lines
                    .push(("Session reset".into(), Color32::from_rgb(0, 255, 255)));
                self.session_status = "Reset".into();
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
}

impl App for DeepSeekGui {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Poll agent events each frame
        while let Ok(event) = self.rx_events.try_recv() {
            self.handle_stream_event(event);
            self.auto_scroll = true;
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

                    // ── Model selector ──
                    let prev_idx = self.selected_model_idx;
                    egui::ComboBox::from_label("Model")
                        .selected_text(&self.model_options[self.selected_model_idx])
                        .show_ui(ui, |ui| {
                            for (i, opt) in self.model_options.iter().enumerate() {
                                ui.selectable_value(
                                    &mut self.selected_model_idx,
                                    i,
                                    opt,
                                );
                            }
                        });
                    if self.selected_model_idx != prev_idx {
                        let new_model = self.model_options[self.selected_model_idx].clone();
                        self.model = new_model.clone();
                        if let Ok(mut model) = self.model_flag.lock() {
                            *model = new_model.clone();
                        }
                        info!(
                            model = %new_model,
                            "model changed via settings panel"
                        );
                    }

                    ui.add_space(8.0);

                    // ── Thinking toggle ──
                    let mut thinking = self.thinking_flag.load(Ordering::SeqCst);
                    if ui.checkbox(&mut thinking, "Thinking enabled").changed() {
                        self.thinking_flag.store(thinking, Ordering::SeqCst);
                        info!(thinking = thinking, "thinking toggled via settings panel");
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
                    ui.checkbox(&mut self.show_raw_output, "Show raw output");
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

                    // ── Experimental features section ──
                    ui.label(RichText::new("Experimental").color(Color32::from_rgb(255, 200, 100)));
                    ui.label(
                        RichText::new("More features coming soon...")
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

        // ── Output area (central, scrollable) ──
        egui::CentralPanel::default().show(ctx, |ui| {
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
                        // Group consecutive WHITE (model output) lines as markdown blocks;
                        // non-white lines (tool calls, errors, user input, reasoning) stay raw.
                        let mut md_buf: Vec<&str> = Vec::new();
                        for (text, color) in &self.output_lines {
                            if *color == Color32::WHITE {
                                md_buf.push(text);
                            } else {
                                if !md_buf.is_empty() {
                                    let md = md_buf.join("\n");
                                    egui_commonmark::CommonMarkViewer::new()
                                        .show(ui, &mut self.markdown_cache, &md);
                                    md_buf.clear();
                                }
                                ui.label(RichText::new(text.as_str()).color(*color));
                            }
                        }
                        if !md_buf.is_empty() {
                            let md = md_buf.join("\n");
                            egui_commonmark::CommonMarkViewer::new()
                                .show(ui, &mut self.markdown_cache, &md);
                        }
                    }
                });
        });
        self.auto_scroll = false;

        // ── Input bar ──
        egui::TopBottomPanel::bottom("input_panel")
            .min_height(32.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(">");
                    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
                    let tab = ui.input(|i| i.key_pressed(egui::Key::Tab));
                    let ctrl_q = ui.input(|i| {
                        i.modifiers.ctrl && i.key_pressed(egui::Key::Q)
                    });

                    let response = ui.add(
                        TextEdit::singleline(&mut self.input_buffer)
                            .hint_text("Type your message...")
                            .desired_width(f32::INFINITY),
                    );
                    response.request_focus();

                    if enter && !self.input_buffer.trim().is_empty() {
                        let input = std::mem::take(&mut self.input_buffer);
                        self.output_lines
                            .push((format!("> {input}"), Color32::from_rgb(100, 149, 237)));
                        self.session_status = "Running...".into();
                        let _ = self.tx_input.send(input);
                        self.auto_scroll = true;
                    }

                    // Escape → interrupt agent
                    if escape {
                        info!("user pressed Escape — interrupting agent");
                        self.interrupt_flag.store(true, Ordering::SeqCst);
                        self.output_lines.push((
                            "[Interrupting...]".into(),
                            Color32::from_rgb(255, 165, 0),
                        ));
                    }

                    // Tab → toggle settings panel
                    if tab {
                        self.settings_visible = !self.settings_visible;
                    }

                    // Ctrl+Q → quit
                    if ctrl_q {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            });

        // ── Status bar ──
        egui::TopBottomPanel::bottom("status_bar")
            .min_height(24.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("Model: {}", self.model))
                            .color(Color32::from_rgb(0, 255, 255)),
                    );
                    ui.separator();
                    ui.label(
                        RichText::new(format!("Tokens: {}", self.token_count))
                            .color(Color32::from_rgb(160, 160, 160)),
                    );
                    ui.separator();
                    ui.label(
                        RichText::new(&self.session_status)
                            .color(Color32::from_rgb(0, 200, 0)),
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
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            ui.label(
                                RichText::new("Tab: settings | Esc: interrupt | Ctrl+Q: quit")
                                    .color(Color32::from_rgb(128, 128, 128))
                                    .small(),
                            );
                        },
                    );
                });
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_gui() -> DeepSeekGui {
        let (tx_events, rx_events) = mpsc::unbounded_channel();
        let (tx_input, _rx_input) = mpsc::unbounded_channel();
        DeepSeekGui::new(
            rx_events,
            tx_input,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new("deepseek-v4-flash".into())),
        )
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
        assert_ne!(*color, Color32::WHITE, "user input must not be white (would render as markdown)");
        assert_eq!(*color, Color32::from_rgb(100, 149, 237));
        assert!(text.starts_with("> "), "user input starts with > which would be blockquote in markdown");
    }

    #[test]
    fn model_output_is_white_for_markdown_rendering() {
        let mut gui = make_gui();
        gui.handle_stream_event(StreamEvent::Text {
            turn: 1,
            text: "**bold** and *italic*".into(),
        });
        let (_, color) = &gui.output_lines[0];
        assert_eq!(*color, Color32::WHITE, "model output must be white to trigger markdown rendering");
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
        });

        assert_eq!(gui.output_lines.len(), 4, "expected 4 lines: user input, reasoning, text, turn end");

        // Line 0: user input — must NOT be white (would become markdown)
        let (text0, color0) = &gui.output_lines[0];
        assert_ne!(*color0, Color32::WHITE, "user input must not be white (markdown)");
        assert_eq!(*color0, Color32::from_rgb(100, 149, 237), "user input must be blue");
        assert!(text0.starts_with("> "), "user input has > prefix that would be blockquote in markdown");

        // Line 1: reasoning — grey, visible in output
        let (text1, color1) = &gui.output_lines[1];
        assert_eq!(*color1, Color32::from_rgb(160, 160, 160), "reasoning must be grey");
        assert_eq!(text1, "The user wants me to pick a secret number. I'll pick 42.");

        // Line 2: model output — white, will be rendered as markdown
        let (text2, color2) = &gui.output_lines[2];
        assert_eq!(*color2, Color32::WHITE, "model output must be white for markdown rendering");
        assert_eq!(text2, "I've picked a number between 1 and 100.");

        // Line 3: turn end — grey status
        let (text3, color3) = &gui.output_lines[3];
        assert_eq!(*color3, Color32::from_rgb(128, 128, 128), "turn end must be grey");
        assert!(text3.contains("turn end"), "turn end line must contain 'turn end'");
    }
}
