//! Event routing and main-session side effects.
//!
//! A `RoutedEvent` with an empty route applies side effects (token
//! counters, cache stats, autosave, voice, autopilot progress) then
//! lands in the transcript. A non-empty route skips every side
//! effect and only reaches the transcript's nested subagent block.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};

use crate::agent::agent_loop::{RoutedEvent, StreamEvent};
use crate::voice::service::VoiceCommand;

use super::transcript::{BlockKind, Severity};
use super::voice_ui::PttKeys;
use super::DeepSeekGui;

/// How often the timed save fires while a turn runs.
const TIMED_SAVE_INTERVAL: Duration = Duration::from_secs(15);

impl DeepSeekGui {
    /// Route one event. Empty route triggers side effects.
    pub(super) fn dispatch_event(
        &mut self,
        routed: RoutedEvent,
    ) {
        if routed.route.is_empty() {
            self.process_main_event(&routed.event);
        }
        self.transcript.apply_routed_event(routed);
        self.unsaved_changes = true;
        self.follow_output = true;
    }

    /// Side effects for a main-session event only.
    fn process_main_event(&mut self, event: &StreamEvent) {
        match event {
            StreamEvent::Text { text, .. } => {
                self.voice.push_reply_text(text);
            }
            StreamEvent::TurnEnd {
                total_tokens,
                prompt_cache_hit_tokens,
                prompt_cache_miss_tokens,
                ..
            } => self.on_turn_end(
                *total_tokens,
                *prompt_cache_hit_tokens,
                *prompt_cache_miss_tokens,
            ),
            StreamEvent::ConversationSnapshot {
                messages,
                claude_session_id,
            } => {
                self.sessions.record_snapshot(
                    messages,
                    claude_session_id,
                );
            }
            StreamEvent::Interrupted { .. } => {
                self.session_status = "Interrupted".into();
                self.voice.clear_reply();
            }
            StreamEvent::SessionReset => self.on_session_reset(),
            StreamEvent::RepeatIterationStart {
                index, total, ..
            } => self.on_repeat_iter(*index, *total),
            StreamEvent::RepeatFinished {
                completed,
                total,
            } => {
                self.autopilot.set_finished(*completed, *total);
            }
            StreamEvent::ToolCallStart { tool, args, .. } => {
                info!(tool=%tool, args=%args, "tool call start");
            }
            StreamEvent::ToolCallEnd {
                tool, output, is_error, ..
            } => {
                if *is_error {
                    warn!(tool=%tool, error=%output, "tool call failed");
                } else {
                    debug!(tool=%tool, "tool call ok");
                }
            }
            StreamEvent::Reasoning { .. }
            | StreamEvent::Error { .. } => {}
        }
    }

    fn on_turn_end(
        &mut self,
        total_tokens: usize,
        cache_hit: u32,
        cache_miss: u32,
    ) {
        self.token_count = total_tokens.to_string();
        self.total_cache_hit_tokens += cache_hit;
        self.total_cache_miss_tokens += cache_miss;
        self.voice.speak_accumulated_reply();
        let origin = self.current_origin();
        self.sessions.autosave(
            &mut self.transcript,
            origin,
        );
        self.unsaved_changes = false;
        self.saved_at = Instant::now();
        self.session_status = "Ready".into();
    }

    fn on_session_reset(&mut self) {
        let origin = self.current_origin();
        self.sessions.save_outgoing_and_start_new(
            &mut self.transcript,
            origin,
        );
        self.total_cache_hit_tokens = 0;
        self.total_cache_miss_tokens = 0;
        self.voice.clear_reply();
    }

    fn on_repeat_iter(&mut self, index: u32, total: u32) {
        let origin = self.current_origin();
        self.sessions.save_outgoing_and_start_new(
            &mut self.transcript,
            origin,
        );
        self.autopilot.set_running(index, total);
    }

    /// Write a dirty session once per interval while a turn runs.
    pub(super) fn check_timed_save(&mut self) {
        if !self.unsaved_changes {
            return;
        }
        if self.saved_at.elapsed() < TIMED_SAVE_INTERVAL {
            return;
        }
        let origin = self.current_origin();
        self.sessions.autosave(
            &mut self.transcript,
            origin,
        );
        self.unsaved_changes = false;
        self.saved_at = Instant::now();
    }

    pub(super) fn drain_events(&mut self) {
        while let Ok(routed) = self.rx_events.try_recv() {
            self.dispatch_event(routed);
        }
    }

    pub(super) fn drain_voice(&mut self) {
        let events = self.voice.drain_events();
        for event in events {
            let text = self.voice.handle_event(
                event, &mut self.transcript,
            );
            if let Some(text) = text {
                self.input_buffer = text;
                self.send_input();
            }
        }
    }

    pub(super) fn drain_model_lists(&mut self) {
        let lists = self.backends.drain_fetched_lists();
        for (name, models) in lists {
            self.backends.apply_fetched_list(&name, models);
        }
    }

    pub(super) fn handle_global_keys(
        &mut self,
        ctx: &eframe::egui::Context,
    ) {
        let tab = ctx.input(|i| {
            i.key_pressed(eframe::egui::Key::Tab)
        });
        let quit = ctx.input(|i| {
            i.modifiers.ctrl
                && i.key_pressed(eframe::egui::Key::Q)
        });
        let esc = ctx.input(|i| {
            i.key_pressed(eframe::egui::Key::Escape)
        });
        if tab {
            self.settings_visible = !self.settings_visible;
        }
        if quit {
            ctx.send_viewport_cmd(
                eframe::egui::ViewportCommand::Close,
            );
        }
        if esc {
            self.handles.interrupt.store(true, Ordering::SeqCst);
            self.autopilot.request_stop();
            self.voice.send(VoiceCommand::StopSpeaking);
            self.transcript.push(BlockKind::Notice {
                text: "[Interrupting...]".into(),
                severity: Severity::Warning,
            });
        }
    }

    pub(super) fn handle_ptt(
        &mut self,
        ctx: &eframe::egui::Context,
    ) {
        let any_focused = ctx.memory(|mem| mem.focused().is_some());
        let keys = ctx.input(|i| PttKeys {
            space_pressed: i.key_pressed(eframe::egui::Key::Space),
            space_released: i.key_released(eframe::egui::Key::Space),
            ctrl_held: i.modifiers.ctrl,
            ctrl_space_pressed: i.modifiers.ctrl
                && i.key_pressed(eframe::egui::Key::Space),
            input_focused: any_focused,
        });
        self.voice.handle_ptt(keys, self.settings_visible);
    }
}
