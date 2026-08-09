//! Native GUI built on egui/eframe. `DeepSeekGui` implements
//! `eframe::App` and owns the transcript, the input bar, the
//! settings sidebar, and the three tabs (Chat, Autopilot,
//! Sessions). Rendering, event dispatch, format helpers, and
//! test accessors each live in their own submodule.

pub mod agent_handles;
pub mod attachment;
pub mod autopilot_tab;
pub mod backend_picker;
mod draw;
mod event_dispatch;
mod format;
mod panels;
pub mod session_state;
pub mod sessions_tab;
pub mod settings_panel;
#[cfg(feature = "test-support")]
mod test_access;
pub mod transcript;
pub mod voice_ui;

pub use format::{
    BLOCK_GAP, IMAGE_LABEL_COLOR, TOOL_ERROR_COLOR, TURN_GAP, block_color, format_elapsed_ms,
    gap_before, raw_block_text, raw_span_text, role_label, severity_color, subagent_elapsed_ms,
    subagent_header_summary, subagent_state_color, tool_color, tool_output_color, tool_summary,
    truncate_args,
};

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use egui_commonmark::CommonMarkCache;
use tokio::sync::mpsc;
use tracing::warn;

use crate::agent::agent_loop::{AgentCommand, RoutedEvent};
use crate::agent::repeat::RepeatCommand;
use crate::config::settings::Settings;
use crate::effort::Effort;
use crate::session::SessionStore;
use crate::voice::service::{VoiceCommand, VoiceEvent};

use agent_handles::AgentHandles;
use attachment::AttachmentSlot;
use autopilot_tab::AutopilotTab;
use backend_picker::{BackendPicker, BackendSwitch};
use session_state::{SessionOrigin, SessionState};
use transcript::{BlockKind, Transcript};
use voice_ui::VoiceUi;

/// Which tab the central panel shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ActiveTab {
    #[default]
    Chat,
    Autopilot,
    Sessions,
}

/// The top-level GUI state. Fields are `pub(super)` so submodules
/// (draw, event_dispatch, settings_panel, sessions_tab,
/// test_access) can reach them without getters.
pub struct DeepSeekGui {
    pub(super) rx_events: mpsc::UnboundedReceiver<RoutedEvent>,
    pub(super) tx_input: mpsc::UnboundedSender<AgentCommand>,
    pub(super) handles: AgentHandles,
    pub(super) settings: Settings,
    pub(super) project_root: PathBuf,
    pub(super) transcript: Transcript,
    pub(super) input_buffer: String,
    pub(super) input_focused: bool,
    pub(super) attachment: AttachmentSlot,
    pub(super) backends: BackendPicker,
    pub(super) voice: VoiceUi,
    pub(super) autopilot: AutopilotTab,
    pub(super) sessions: SessionState,
    pub(super) active_tab: ActiveTab,
    pub(super) settings_visible: bool,
    pub(super) show_raw_output: bool,
    pub(super) effort: Effort,
    pub(super) context_budget: usize,
    pub(super) working_dir_buffer: String,
    pub(super) token_count: String,
    pub(super) total_cache_hit_tokens: u32,
    pub(super) total_cache_miss_tokens: u32,
    pub(super) session_status: String,
    pub(super) follow_output: bool,
    pub(super) unsaved_changes: bool,
    pub(super) saved_at: Instant,
    pub(super) md_cache: CommonMarkCache,
}

impl DeepSeekGui {
    pub fn new(
        rx_events: mpsc::UnboundedReceiver<RoutedEvent>,
        tx_input: mpsc::UnboundedSender<AgentCommand>,
        handles: AgentHandles,
        settings: Settings,
        project_root: PathBuf,
    ) -> Self {
        let effort = Effort::load(&handles.effort);
        let context_budget = handles.context_budget.load(Ordering::SeqCst);
        let working_dir_buffer = handles.working_dir.lock().unwrap().display().to_string();
        let show_raw = settings.show_raw_output();
        let store = SessionStore::for_project(&project_root);
        let origin = SessionOrigin {
            backend: String::new(),
            model: handles.model.lock().unwrap().clone(),
        };
        Self {
            backends: BackendPicker::new(&settings, Arc::clone(&handles.model)),
            voice: VoiceUi::new(&settings, &handles.voice_mode),
            autopilot: AutopilotTab::new(&settings, &project_root),
            sessions: SessionState::new(store, origin),
            rx_events,
            tx_input,
            handles,
            settings,
            project_root,
            transcript: Transcript::new(),
            input_buffer: String::new(),
            input_focused: false,
            attachment: AttachmentSlot::new(),
            active_tab: ActiveTab::default(),
            settings_visible: false,
            show_raw_output: show_raw,
            effort,
            context_budget,
            working_dir_buffer,
            token_count: "0".into(),
            total_cache_hit_tokens: 0,
            total_cache_miss_tokens: 0,
            session_status: "Ready".into(),
            follow_output: false,
            unsaved_changes: false,
            saved_at: Instant::now(),
            md_cache: CommonMarkCache::default(),
        }
    }

    /// Attach the autopilot repeat channel and its interrupt flag.
    pub fn with_repeat(
        mut self,
        tx: mpsc::UnboundedSender<RepeatCommand>,
        flag: Arc<AtomicBool>,
    ) -> Self {
        self.autopilot.attach(tx, flag);
        self
    }

    /// Attach a running voice service's channels.
    pub fn with_voice(
        mut self,
        rx: mpsc::UnboundedReceiver<VoiceEvent>,
        tx: mpsc::UnboundedSender<VoiceCommand>,
    ) -> Self {
        self.voice.attach(rx, tx);
        self
    }

    // ── Internal helpers called from siblings ──

    pub(super) fn persist_settings(&self) {
        if let Err(e) = self.settings.save(&self.project_root) {
            warn!(error = %e, "failed to persist settings");
        }
    }

    pub(super) fn apply_backend_switch(&mut self, switch: BackendSwitch) {
        self.sessions
            .save_outgoing_and_start_new(&mut self.transcript, switch.outgoing);
        let _ = self.tx_input.send(switch.command);
    }

    pub(super) fn current_origin(&self) -> SessionOrigin {
        SessionOrigin {
            backend: self.backends.active_backend().to_string(),
            model: self.backends.model().to_string(),
        }
    }

    pub(super) fn start_new_session(&mut self) {
        let origin = self.current_origin();
        let cmd = self.sessions.start_new(&mut self.transcript, origin);
        let _ = self.tx_input.send(cmd);
    }

    pub(super) fn load_session(&mut self, id: crate::session::SessionId) {
        let origin = self.current_origin();
        if let Some(cmd) = self.sessions.load(id, &mut self.transcript, origin) {
            let _ = self.tx_input.send(cmd);
        }
    }

    pub(super) fn send_input(&mut self) {
        let has_text = !self.input_buffer.trim().is_empty();
        let has_image = !self.attachment.is_empty();
        if !has_text && !has_image {
            return;
        }
        let text = std::mem::take(&mut self.input_buffer);
        let image = self.attachment.take();
        self.transcript.push(BlockKind::User { text: text.clone() });
        if let Some(ref img) = image {
            self.transcript
                .push(BlockKind::Image { image: img.clone() });
        }
        let _ = self.tx_input.send(AgentCommand::UserTurn { text, image });
        self.session_status = "Running...".into();
    }
}

impl eframe::App for DeepSeekGui {
    fn update(&mut self, ctx: &eframe::egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events();
        self.check_timed_save();
        self.drain_voice();
        self.drain_model_lists();
        self.handle_global_keys(ctx);
        self.attachment.poll_ctrl_v_paste(ctx, &mut self.transcript);
        self.attachment
            .handle_dropped_files(ctx, &mut self.transcript);
        self.render_settings_panel(ctx);
        self.paint_bottom_panels(ctx);
        self.paint_central(ctx);
        self.handle_ptt(ctx);
        self.follow_output = false;
        ctx.request_repaint_after(Duration::from_millis(50));
    }
}
