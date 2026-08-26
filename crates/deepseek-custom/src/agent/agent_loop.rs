use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::api::client::ApiClient;
use crate::api::types::Message;
use crate::backend::SharedFlags;
use crate::backend::registry::SubagentRegistry;
use crate::context::relevance;
use crate::effort::Effort;
use crate::tools::ToolRegistry;

use super::agent_helpers::context_low_water;
use super::agent_style::StyleState;
use super::agent_types::{AgentConfig, DEFAULT_CONTEXT_BUDGET, DEFAULT_TARGET_GRADE, grade_to_u8};
use super::events::{RoutedEvent, StreamEvent};
use super::history::{MessageHistory, PruneReport};
use super::prompt::voice_mode_instructions;

/// Core agent loop: user input -> API call -> tool execution -> repeat.
pub struct AgentLoop {
    pub(crate) client: ApiClient,
    pub(crate) tools: ToolRegistry,
    pub(crate) history: MessageHistory,
    pub(crate) config: AgentConfig,
    pub(crate) tx_events: Option<mpsc::UnboundedSender<RoutedEvent>>,
    pub(crate) interrupt_flag: Arc<AtomicBool>,
    pub(crate) effort_flag: Arc<AtomicU8>,
    pub(crate) model_name: Arc<Mutex<String>>,
    pub(crate) voice_mode_flag: Arc<AtomicBool>,
    pub(crate) context_budget: Arc<AtomicUsize>,
    pub(crate) repeat_interrupt_flag: Arc<AtomicBool>,
    pub(crate) subagent_registry: Option<Arc<SubagentRegistry>>,
    pub(crate) working_dir: Option<Arc<Mutex<PathBuf>>>,
    pub(crate) style_state: StyleState,
    pub(crate) style_critic_backend: Option<String>,
}

impl AgentLoop {
    pub fn new(
        client: ApiClient,
        tools: ToolRegistry,
        system_prompt: String,
        config: AgentConfig,
        interrupt_flag: Arc<AtomicBool>,
    ) -> Self {
        let effort = config.effort;
        let model = config.model.clone();
        let style_plain = Arc::new(AtomicBool::new(false));
        let style_grade = Arc::new(AtomicU8::new(DEFAULT_TARGET_GRADE));
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
            style_state: StyleState {
                plain_language_flag: style_plain,
                target_grade_flag: style_grade,
                grade_tolerance: 2.0,
                max_revise_attempts: 2,
            },
            style_critic_backend: None,
        }
    }

    pub fn set_event_sender(&mut self, tx: mpsc::UnboundedSender<RoutedEvent>) {
        self.tx_events = Some(tx);
    }

    pub fn set_subagent_registry(&mut self, registry: Arc<SubagentRegistry>) {
        self.subagent_registry = Some(registry);
    }

    pub fn set_working_dir(&mut self, working_dir: Arc<Mutex<PathBuf>>) {
        self.working_dir = Some(working_dir);
    }

    pub fn set_style_config(
        &mut self,
        plain_language_enabled: bool,
        target_grade: f32,
        grade_tolerance: f32,
        max_revise_attempts: u32,
        critic_backend: Option<String>,
    ) {
        self.style_state
            .plain_language_flag
            .store(plain_language_enabled, Ordering::SeqCst);
        self.style_state
            .target_grade_flag
            .store(grade_to_u8(target_grade), Ordering::SeqCst);
        self.style_state.grade_tolerance = grade_tolerance;
        self.style_state.max_revise_attempts = max_revise_attempts;
        self.style_critic_backend = critic_backend;
    }

    pub fn style_plain_language_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.style_state.plain_language_flag)
    }

    pub fn style_target_grade_flag(&self) -> Arc<AtomicU8> {
        Arc::clone(&self.style_state.target_grade_flag)
    }

    pub fn set_effort_flag(&mut self, effort_flag: Arc<AtomicU8>) {
        self.effort_flag = effort_flag;
    }

    pub fn adopt_flags(&mut self, flags: &SharedFlags) {
        self.interrupt_flag = Arc::clone(&flags.interrupt);
        self.model_name = Arc::clone(&flags.model);
        self.voice_mode_flag = Arc::clone(&flags.voice_mode);
        self.context_budget = Arc::clone(&flags.context_budget);
        self.repeat_interrupt_flag = Arc::clone(&flags.repeat_interrupt);
        self.style_state.plain_language_flag = Arc::clone(&flags.style_plain_language);
        self.style_state.target_grade_flag = Arc::clone(&flags.style_target_grade);
    }

    #[cfg(feature = "test-support")]
    pub fn subagent_registry_for_test(&self) -> Option<Arc<SubagentRegistry>> {
        self.subagent_registry.clone()
    }

    pub fn interrupt_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.interrupt_flag)
    }

    pub fn effort_flag(&self) -> Arc<AtomicU8> {
        Arc::clone(&self.effort_flag)
    }

    pub fn model_flag(&self) -> Arc<Mutex<String>> {
        Arc::clone(&self.model_name)
    }

    pub fn voice_mode_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.voice_mode_flag)
    }

    pub fn context_budget_flag(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.context_budget)
    }

    pub fn repeat_interrupt_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.repeat_interrupt_flag)
    }

    #[cfg(feature = "test-support")]
    pub fn tool_names(&self) -> Vec<String> {
        self.tools
            .list()
            .iter()
            .map(|t| t.name().to_string())
            .collect()
    }

    pub fn clear_history(&mut self) {
        self.history = MessageHistory::new(self.history.system_prompt().to_string());
    }

    pub fn restore_history(&mut self, messages: Vec<Message>) {
        self.history.restore(messages);
    }

    pub(crate) fn sync_dynamic_config(&mut self) {
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
        if let Some(working_dir) = &self.working_dir
            && let Ok(dir) = working_dir.lock()
        {
            self.history
                .set_working_dir(Some(dir.display().to_string()));
        }
    }

    #[cfg(feature = "test-support")]
    pub fn sync_dynamic_config_for_test(&mut self) {
        self.sync_dynamic_config();
    }

    pub(crate) fn apply_prune(&mut self, scores: Option<&[f32]>) -> PruneReport {
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

    #[cfg(feature = "test-support")]
    pub fn apply_prune_for_test(&mut self, scores: Option<&[f32]>) -> PruneReport {
        self.apply_prune(scores)
    }

    pub(crate) async fn maybe_prune_context(&mut self) {
        let budget = self.context_budget.load(Ordering::SeqCst);
        if self.history.estimated_tokens() <= budget {
            return;
        }

        let messages: Vec<Message> = self.history.iter().cloned().collect();
        let scores = relevance::score_messages(&self.client, &messages, &self.config.model).await;
        if scores.is_none() {
            warn!(
                "context pruning: relevance scoring failed, \
                 falling back to oldest-first order"
            );
        }
        self.apply_prune(scores.as_deref());
    }

    pub(crate) fn send_event(&self, event: StreamEvent) {
        if let Some(ref tx) = self.tx_events {
            let _ = tx.send(RoutedEvent::own(event));
        }
    }

    pub(crate) fn send_repeat_finished(&self, completed: u32, total: u32) {
        self.send_event(StreamEvent::RepeatFinished { completed, total });
    }

    pub fn history(&self) -> &MessageHistory {
        &self.history
    }

    #[cfg(feature = "test-support")]
    pub fn history_mut(&mut self) -> &mut MessageHistory {
        &mut self.history
    }

    #[cfg(feature = "test-support")]
    pub fn config(&self) -> &AgentConfig {
        &self.config
    }

    #[cfg(feature = "test-support")]
    pub fn maybe_check_plain_language_for_test(&self, text: &str) -> bool {
        self.style_state.needs_revision(text)
    }

    #[cfg(feature = "test-support")]
    pub async fn revise_for_plain_language_for_test(
        &self,
        text: &str,
    ) -> super::agent_style::StyleRevision {
        self.style_state
            .revise(
                &self.client,
                &self.config.model,
                self.config.max_tokens,
                text,
            )
            .await
    }

    pub fn reset(&mut self, new_system_prompt: String, new_user_prompt: String) {
        info!("agent: session reset");
        self.history = MessageHistory::new(new_system_prompt);
        self.history.push(Message::user(new_user_prompt));
        self.send_event(StreamEvent::SessionReset);
    }
}
