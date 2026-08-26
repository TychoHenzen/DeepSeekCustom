//! Test-only accessors for `DeepSeekGui`, gated behind
//! `test-support`. Each method exposes one field or delegates to
//! one internal method, so tests can drive the GUI without a
//! window.

#![cfg(feature = "test-support")]

use std::path::PathBuf;

use tokio::sync::mpsc;

use crate::agent::events::{AgentCommand, RoutedEvent, StreamEvent};
use crate::api::types::ImageAttachment;
use crate::config::settings::Settings;
use crate::effort::Effort;
use crate::session::SessionId;
use crate::voice::service::VoiceEvent;

use super::agent_handles::AgentHandles;
use super::attachment::AttachmentSlot;
use super::autopilot_tab::AutopilotTab;
use super::backend_picker::BackendPicker;
use super::procedure_tab::ProcedureTab;
use super::session_state::SessionState;
use super::transcript::Transcript;
use super::voice_ui::VoiceUi;
use super::{ActiveTab, DeepSeekGui, PendingSwitch};

impl DeepSeekGui {
    pub fn transcript_for_test(&self) -> &Transcript {
        &self.transcript
    }

    pub fn transcript_mut_for_test(&mut self) -> &mut Transcript {
        &mut self.transcript
    }

    pub fn set_attachment_for_test(&mut self, img: ImageAttachment) {
        self.attachment.set(img, &mut self.transcript);
    }

    pub fn attachment_for_test(&self) -> &AttachmentSlot {
        &self.attachment
    }

    pub fn set_input_buffer_for_test(&mut self, text: &str) {
        self.input_buffer = text.to_string();
    }

    pub fn input_buffer_for_test(&self) -> &str {
        &self.input_buffer
    }

    pub fn submit_current_input_for_test(&mut self) {
        self.send_input();
    }

    pub fn set_tx_input_for_test(&mut self, tx: mpsc::UnboundedSender<AgentCommand>) {
        self.tx_input = tx;
    }

    pub fn session_status_for_test(&self) -> &str {
        &self.session_status
    }

    pub fn token_count_for_test(&self) -> &str {
        &self.token_count
    }

    pub fn total_cache_hit_tokens_for_test(&self) -> u32 {
        self.total_cache_hit_tokens
    }

    pub fn total_cache_miss_tokens_for_test(&self) -> u32 {
        self.total_cache_miss_tokens
    }

    pub fn voice_for_test(&self) -> &VoiceUi {
        &self.voice
    }

    pub fn voice_mut_for_test(&mut self) -> &mut VoiceUi {
        &mut self.voice
    }

    pub fn handles_for_test(&self) -> &AgentHandles {
        &self.handles
    }

    pub fn show_raw_output_for_test(&self) -> bool {
        self.show_raw_output
    }

    pub fn handle_voice_event_for_test(&mut self, event: VoiceEvent) {
        let text = self.voice.handle_event(event, &mut self.transcript);
        if let Some(text) = text {
            self.input_buffer = text;
            self.send_input();
        }
    }

    pub fn handle_stream_event(&mut self, event: StreamEvent) {
        self.dispatch_event(RoutedEvent::own(event));
    }

    pub fn handle_routed_event_for_test(&mut self, routed: RoutedEvent) {
        self.dispatch_event(routed);
    }

    pub fn plain_language_for_test(&self) -> bool {
        self.plain_language
    }

    pub fn plain_language_grade_for_test(&self) -> u8 {
        self.plain_language_grade
    }

    pub fn context_budget_for_test(&self) -> usize {
        self.context_budget
    }

    pub fn set_context_budget_for_test(&mut self, budget: usize) {
        self.context_budget = budget;
    }

    pub fn effort_for_test(&self) -> Effort {
        self.effort
    }

    pub fn set_effort_for_test(&mut self, effort: Effort) {
        self.effort = effort;
    }

    pub fn settings_mut_for_test(&mut self) -> &mut Settings {
        &mut self.settings
    }

    pub fn persist_settings_for_test(&self) {
        self.persist_settings();
    }

    pub fn working_dir_display_for_test(&self) -> &str {
        &self.working_dir_display
    }

    pub fn apply_working_dir_selection_for_test(&mut self, selection: Option<PathBuf>) {
        self.apply_working_dir_selection(selection);
    }

    pub fn active_tab_for_test(&self) -> ActiveTab {
        self.active_tab
    }

    pub fn set_active_tab_for_test(&mut self, tab: ActiveTab) {
        self.active_tab = tab;
    }

    pub fn autopilot_for_test(&self) -> &AutopilotTab {
        &self.autopilot
    }

    pub fn sessions_for_test(&self) -> &SessionState {
        &self.sessions
    }

    pub fn start_new_session_for_test(&mut self) {
        self.start_new_session();
    }

    pub fn load_session_for_test(&mut self, id: SessionId) {
        self.load_session(id);
    }

    pub fn delete_saved_session_for_test(&mut self, id: SessionId) {
        self.delete_saved_session(id);
    }

    pub fn backends_mut_for_test(&mut self) -> &mut BackendPicker {
        &mut self.backends
    }

    pub fn procedure_for_test(&self) -> &ProcedureTab {
        &self.procedure
    }

    pub fn procedure_mut_for_test(&mut self) -> &mut ProcedureTab {
        &mut self.procedure
    }

    pub fn drain_procedure_for_test(&mut self) {
        self.procedure.drain_progress();
    }

    /// Whether a turn is in flight. Nothing production reads this back:
    /// the event pump writes it and `defer_switch` consumes it in the same
    /// type, so there is no side-effect-free seam a test could use instead.
    pub fn turn_active_for_test(&self) -> bool {
        self.turn_active
    }

    /// The session switch waiting for the running turn, if any. Test-only
    /// for the same reason as `turn_active_for_test`.
    pub fn pending_switch_for_test(&self) -> Option<PendingSwitch> {
        self.pending_switch.clone()
    }

    /// Send a user turn without a window, so a test can put the GUI into
    /// the turn-running state the deferral depends on.
    pub fn send_input_for_test(&mut self, text: &str) {
        self.input_buffer = text.to_string();
        self.send_input();
    }
}
