//! Owns a long-lived `claude -p` child process: spawns it, feeds it user
//! turns over stdin, and publishes `StreamEvent` values parsed from its
//! stdout on the same channel the agent loop already uses.

mod repeat;
mod spawn;
#[cfg(feature = "test-support")]
mod test_support;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::AsyncWriteExt;
use tokio::process::{Child, ChildStdin};
use tokio::sync::mpsc;

use crate::agent::events::{RoutedEvent, StreamEvent};
use crate::api::types::ImageAttachment;
use crate::effort::Effort;
use crate::error::Result;

use super::args::{INTERRUPT_POLL_INTERVAL, UNUSED_CONTEXT_BUDGET, build_user_turn_line};

/// Owns a `claude -p` child process for the lifetime of a session. It also
/// owns the six shared flags `Backend` exposes to the GUI, in place of the
/// ones `AgentLoop` owns for the API path.
///
/// Two of those flags have no meaning here: `context_budget_flag` and
/// `model_flag`. Claude Code manages its own context compaction, and the
/// model is fixed for the lifetime of the child. The GUI can still write to
/// them. The settings panel does not know which backend is active. Nothing
/// in this driver ever reads them back.
///
/// Most fields are `pub(super)` so sibling modules (`spawn`,
/// `repeat`, `test_support`) can access them without an accessor.
pub struct ClaudeCliDriver {
    pub(super) child: Option<Child>,
    pub(super) stdin: Option<ChildStdin>,
    pub(super) turn_done: Option<mpsc::UnboundedReceiver<()>>,
    pub(super) session_id_rx: Option<mpsc::UnboundedReceiver<String>>,
    pub(super) model: String,
    pub(super) permission_mode: Option<String>,
    pub(super) tools_enabled: bool,
    pub(super) extra_env: Option<std::collections::HashMap<String, String>>,
    pub(super) working_dir: Arc<Mutex<PathBuf>>,
    pub(super) tx_events: mpsc::UnboundedSender<RoutedEvent>,
    pub(super) spawned_voice_mode: bool,
    pub(super) spawned_resume_id: Option<String>,
    pub(super) spawned_working_dir: Option<PathBuf>,
    pub(super) spawned_effort: Effort,
    pub(super) interrupt_flag: Arc<AtomicBool>,
    pub(super) effort_flag: Arc<AtomicU8>,
    pub(super) voice_mode_flag: Arc<AtomicBool>,
    pub(super) context_budget_flag: Arc<AtomicUsize>,
    pub(super) model_flag: Arc<Mutex<String>>,
    pub(super) repeat_interrupt_flag: Arc<AtomicBool>,
    pub(super) claude_session_id: Option<String>,
    pub(super) last_reply: Arc<Mutex<String>>,
}

impl ClaudeCliDriver {
    pub fn new(
        model: String,
        permission_mode: Option<String>,
        extra_env: Option<std::collections::HashMap<String, String>>,
        working_dir: Arc<Mutex<PathBuf>>,
        tx_events: mpsc::UnboundedSender<RoutedEvent>,
    ) -> Self {
        Self::new_with_tools(
            model,
            permission_mode,
            extra_env,
            working_dir,
            tx_events,
            true,
        )
    }

    pub fn new_with_tools(
        model: String,
        permission_mode: Option<String>,
        extra_env: Option<std::collections::HashMap<String, String>>,
        working_dir: Arc<Mutex<PathBuf>>,
        tx_events: mpsc::UnboundedSender<RoutedEvent>,
        tools_enabled: bool,
    ) -> Self {
        let model_flag = Arc::new(Mutex::new(model.clone()));
        Self {
            child: None,
            stdin: None,
            turn_done: None,
            session_id_rx: None,
            model,
            permission_mode,
            tools_enabled,
            extra_env,
            working_dir,
            tx_events,
            spawned_voice_mode: false,
            spawned_resume_id: None,
            spawned_working_dir: None,
            spawned_effort: Effort::None,
            interrupt_flag: Arc::new(AtomicBool::new(false)),
            effort_flag: Arc::new(AtomicU8::new(Effort::None.to_u8())),
            voice_mode_flag: Arc::new(AtomicBool::new(false)),
            context_budget_flag: Arc::new(AtomicUsize::new(UNUSED_CONTEXT_BUDGET)),
            model_flag,
            repeat_interrupt_flag: Arc::new(AtomicBool::new(false)),
            claude_session_id: None,
            last_reply: Arc::new(Mutex::new(String::new())),
        }
    }

    pub fn set_claude_session_id(&mut self, id: Option<String>) {
        self.claude_session_id = id;
    }

    pub fn adopt_flags(&mut self, flags: &crate::backend::SharedFlags) {
        self.interrupt_flag = Arc::clone(&flags.interrupt);
        self.effort_flag = Arc::clone(&flags.effort);
        self.voice_mode_flag = Arc::clone(&flags.voice_mode);
        self.context_budget_flag = Arc::clone(&flags.context_budget);
        self.model_flag = Arc::clone(&flags.model);
        self.repeat_interrupt_flag = Arc::clone(&flags.repeat_interrupt);
    }

    pub fn claude_session_id(&self) -> Option<&str> {
        self.claude_session_id.as_deref()
    }

    pub fn child_pid(&self) -> Option<u32> {
        self.child.as_ref().and_then(|child| child.id())
    }

    pub fn interrupt_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.interrupt_flag)
    }

    pub fn effort_flag(&self) -> Arc<AtomicU8> {
        Arc::clone(&self.effort_flag)
    }

    pub fn voice_mode_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.voice_mode_flag)
    }

    pub fn context_budget_flag(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.context_budget_flag)
    }

    pub fn model_flag(&self) -> Arc<Mutex<String>> {
        Arc::clone(&self.model_flag)
    }

    pub fn repeat_interrupt_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.repeat_interrupt_flag)
    }

    pub async fn send(&mut self, text: &str) -> Result<()> {
        self.send_with_image(text, None).await
    }

    pub async fn send_with_image(
        &mut self,
        text: &str,
        image: Option<&ImageAttachment>,
    ) -> Result<()> {
        self.interrupt_flag.store(false, Ordering::SeqCst);
        if let Ok(mut reply) = self.last_reply.lock() {
            reply.clear();
        }
        self.ensure_ready().await?;
        let line = build_user_turn_line(text, image);
        let stdin = self
            .stdin
            .as_mut()
            .expect("ensure_ready always leaves stdin populated on success");
        stdin.write_all(line.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        self.await_turn_end().await;
        self.drain_session_id();
        Ok(())
    }

    fn drain_session_id(&mut self) {
        let Some(rx) = self.session_id_rx.as_mut() else {
            return;
        };
        if let Ok(id) = rx.try_recv() {
            self.claude_session_id = Some(id.clone());
            self.spawned_resume_id = Some(id);
        }
    }

    pub(super) async fn await_turn_end(&mut self) {
        loop {
            if self.interrupt_flag.load(Ordering::SeqCst) {
                self.interrupt();
                self.interrupt_flag.store(false, Ordering::SeqCst);
                return;
            }
            let Some(turn_done) = self.turn_done.as_mut() else {
                return;
            };
            match tokio::time::timeout(INTERRUPT_POLL_INTERVAL, turn_done.recv()).await {
                Ok(Some(())) => return,
                Ok(None) => {
                    self.turn_done = None;
                    return;
                }
                Err(_timed_out) => continue,
            }
        }
    }

    pub fn interrupt(&mut self) {
        if let Some(mut child) = self.child.take()
            && let Err(e) = child.start_kill()
        {
            tracing::warn!("claude_cli: failed to kill child on interrupt: {e}");
        }
        self.stdin = None;
        self.turn_done = None;
        self.session_id_rx = None;
        let _ = self
            .tx_events
            .send(RoutedEvent::own(StreamEvent::Interrupted {
                message: "Interrupted by user (Escape)".into(),
            }));
    }

    pub async fn shutdown(&mut self) {
        if let Some(mut stdin) = self.stdin.take()
            && let Err(e) = stdin.shutdown().await
        {
            tracing::warn!("claude_cli: failed to close stdin on shutdown: {e}");
        }
        if let Some(mut child) = self.child.take()
            && let Err(e) = child.wait().await
        {
            tracing::warn!("claude_cli: failed waiting for child exit: {e}");
        }
        self.turn_done = None;
        self.session_id_rx = None;
    }
}
