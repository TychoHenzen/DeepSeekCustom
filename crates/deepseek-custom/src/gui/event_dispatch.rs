//! Event routing and main-session side effects.
//!
//! A `RoutedEvent` with an empty route applies side effects (token
//! counters, cache stats, autosave, voice, autopilot progress) then
//! lands in the transcript. A non-empty route skips every side
//! effect and only reaches the transcript's nested subagent block.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};

use crate::agent::events::{RoutedEvent, StreamEvent};
use crate::search::SearchKind;
use crate::voice::service::VoiceCommand;

use super::DeepSeekGui;
use super::search_view::SearchProgress;
use super::transcript::{BlockKind, Severity};
use super::voice_ui::PttKeys;

/// How often the timed save fires while a turn runs.
const TIMED_SAVE_INTERVAL: Duration = Duration::from_secs(15);

impl DeepSeekGui {
    /// Whether this event ends the running turn, so a held session switch
    /// may apply.
    ///
    /// `Error` is deliberately not on the list. It fires mid-turn as well:
    /// a turn that carries an image onto a backend that cannot take one
    /// reports the dropped attachment this way, before the request even
    /// goes out. See `build_user_content` in `src/agent/agent_loop.rs`.
    /// Treating it as terminal would end the turn on a notice.
    ///
    /// One iteration's `TurnEnd` does not end an autopilot run. The next
    /// iteration starts immediately and rotates to a fresh conversation of
    /// its own, so a switch released there would be thrown away a moment
    /// later. A run ends on `RepeatFinished`, which fires whether the run
    /// finished its iterations or Escape stopped it.
    fn event_ends_turn(&self, event: &StreamEvent) -> bool {
        match event {
            StreamEvent::Interrupted { .. } | StreamEvent::RepeatFinished { .. } => true,
            StreamEvent::TurnEnd { .. } => !self.autopilot.is_running(),
            _ => false,
        }
    }

    /// Route one event. Empty route triggers side effects.
    pub(super) fn dispatch_event(&mut self, routed: RoutedEvent) {
        let ends_turn = routed.route.is_empty() && self.event_ends_turn(&routed.event);
        if routed.route.is_empty() {
            self.process_main_event(&routed.event);
        }
        self.transcript.apply_routed_event(routed);
        self.unsaved_changes = true;
        self.follow_output = true;
        // A held session switch applies here, after the terminal event has
        // reached the transcript and after `on_turn_end` has saved it, so
        // the outgoing conversation is written whole before the switch
        // replaces it.
        if ends_turn {
            self.turn_active = false;
            self.apply_pending_switch();
        }
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
                self.sessions.record_snapshot(messages, claude_session_id);
            }
            StreamEvent::Interrupted { .. } => {
                self.session_status = "Interrupted".into();
                self.voice.clear_reply();
            }
            StreamEvent::SessionReset => self.on_session_reset(),
            StreamEvent::RepeatIterationStart { index, total, .. } => {
                self.on_repeat_iter(*index, *total)
            }
            StreamEvent::RepeatFinished { completed, total } => {
                self.autopilot.set_finished(*completed, *total);
            }
            StreamEvent::ToolCallStart { tool, args, .. } => {
                info!(tool=%tool, args=%args, "tool call start");
            }
            StreamEvent::ToolCallEnd {
                tool,
                output,
                is_error,
                ..
            } => {
                if *is_error {
                    warn!(tool=%tool, error=%output, "tool call failed");
                } else {
                    debug!(tool=%tool, "tool call ok");
                }
            }
            StreamEvent::SearchProgress(snapshot) => {
                self.on_search_progress(SearchProgress::Running(snapshot.clone()), snapshot.kind);
            }
            StreamEvent::SearchFinished {
                kind,
                summary,
                is_error,
            } => {
                self.on_search_progress(
                    SearchProgress::Finished {
                        summary: summary.clone(),
                        is_error: *is_error,
                    },
                    *kind,
                );
            }
            StreamEvent::Reasoning { .. }
            | StreamEvent::Error { .. }
            | StreamEvent::Info { .. } => {}
        }
    }

    /// Move whichever search tab owns this run to its new state. The kind
    /// on the event decides, rather than which tab happens to be open, so a
    /// run keeps reporting to its own tab while the user reads another.
    fn on_search_progress(&mut self, progress: SearchProgress, kind: SearchKind) {
        match kind {
            SearchKind::Cascade => self.cascade.set_progress(progress),
            SearchKind::Evolve => self.evolve.set_progress(progress),
        }
    }

    fn on_turn_end(&mut self, total_tokens: usize, cache_hit: u32, cache_miss: u32) {
        self.token_count = total_tokens.to_string();
        self.total_cache_hit_tokens += cache_hit;
        self.total_cache_miss_tokens += cache_miss;
        self.voice.speak_accumulated_reply();
        let origin = self.current_origin();
        self.sessions.autosave(&mut self.transcript, origin);
        self.unsaved_changes = false;
        self.saved_at = Instant::now();
        self.session_status = "Ready".into();
    }

    fn on_session_reset(&mut self) {
        let origin = self.current_origin();
        self.sessions
            .save_outgoing_and_start_new(&mut self.transcript, origin);
        self.total_cache_hit_tokens = 0;
        self.total_cache_miss_tokens = 0;
        self.voice.clear_reply();
    }

    fn on_repeat_iter(&mut self, index: u32, total: u32) {
        let origin = self.current_origin();
        self.sessions
            .save_outgoing_and_start_new(&mut self.transcript, origin);
        self.autopilot.set_running(index, total);
        // An autopilot iteration is a turn the GUI never sent, so nothing
        // else would mark one as running. Without this, a session switch
        // during a run would apply mid-iteration.
        self.turn_active = true;
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
        self.sessions.autosave(&mut self.transcript, origin);
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
            let text = self.voice.handle_event(event, &mut self.transcript);
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

    pub(super) fn handle_global_keys(&mut self, ctx: &eframe::egui::Context) {
        let tab = ctx.input(|i| i.key_pressed(eframe::egui::Key::Tab));
        let quit = ctx.input(|i| i.modifiers.ctrl && i.key_pressed(eframe::egui::Key::Q));
        let esc = ctx.input(|i| i.key_pressed(eframe::egui::Key::Escape));
        if tab {
            self.settings_visible = !self.settings_visible;
        }
        if quit {
            ctx.send_viewport_cmd(eframe::egui::ViewportCommand::Close);
        }
        if esc {
            self.handles.interrupt.store(true, Ordering::SeqCst);
            self.autopilot.request_stop();
            // One Escape stops whatever the session is doing, a running
            // search included. Both tabs share one flag, so either call
            // stops the run that is going.
            self.cascade.request_stop();
            self.voice.send(VoiceCommand::StopSpeaking);
            self.transcript.push(BlockKind::Notice {
                text: "[Interrupting...]".into(),
                severity: Severity::Warning,
            });
        }
    }

    pub(super) fn handle_ptt(&mut self, ctx: &eframe::egui::Context) {
        let any_focused = ctx.memory(|mem| mem.focused().is_some());
        let keys = ctx.input(|i| PttKeys {
            space_pressed: i.key_pressed(eframe::egui::Key::Space),
            space_released: i.key_released(eframe::egui::Key::Space),
            ctrl_held: i.modifiers.ctrl,
            ctrl_space_pressed: i.modifiers.ctrl && i.key_pressed(eframe::egui::Key::Space),
            input_focused: any_focused,
        });
        self.voice.handle_ptt(keys, self.settings_visible);
    }
}
