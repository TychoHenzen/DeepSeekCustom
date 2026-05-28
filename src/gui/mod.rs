use std::time::Duration;

use eframe::egui::{self, Color32, RichText, ScrollArea, TextEdit};
use eframe::App;
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
}

impl DeepSeekGui {
    pub fn new(
        rx_events: mpsc::UnboundedReceiver<StreamEvent>,
        tx_input: mpsc::UnboundedSender<String>,
    ) -> Self {
        Self {
            output_lines: Vec::new(),
            input_buffer: String::new(),
            model: "deepseek-v4-flash".into(),
            token_count: "0".into(),
            session_status: "Ready".into(),
            rx_events,
            tx_input,
            auto_scroll: false,
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
                        let can_append = self.output_lines.last()
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
                    for (text, color) in &self.output_lines {
                        ui.label(RichText::new(text).color(*color));
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
                    let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
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
                        let _ = self.tx_input.send(input);
                        self.auto_scroll = true;
                    }
                    if esc {
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
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            ui.label(
                                RichText::new("Esc: quit")
                                    .color(Color32::from_rgb(128, 128, 128))
                                    .small(),
                            );
                        },
                    );
                });
            });
    }
}
