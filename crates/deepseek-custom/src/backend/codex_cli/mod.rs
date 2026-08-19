//! One-shot `codex exec --json` backend driver.

pub mod events;
pub mod map;
mod repeat;
mod spawn;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Child;
use tokio::sync::mpsc::UnboundedSender;

use crate::agent::agent_types::DEFAULT_CONTEXT_BUDGET;
use crate::agent::events::{RoutedEvent, StreamEvent};
use crate::api::types::ImageAttachment;
use crate::backend::SharedFlags;
use crate::effort::Effort;
use crate::error::{HarnessError, Result};

use self::events::{CodexEvent, parse_event};
use self::map::EventMapper;
use self::spawn::{build_args, spawn_codex};

const INTERRUPT_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Holds conversation identity and the shared controls for Codex CLI turns.
pub struct CodexCliDriver {
    thread_id: Option<String>,
    sandbox: Option<String>,
    extra_env: Option<HashMap<String, String>>,
    working_dir: Arc<Mutex<PathBuf>>,
    tx_events: UnboundedSender<RoutedEvent>,
    child: Option<Child>,
    interrupt_flag: Arc<AtomicBool>,
    effort_flag: Arc<AtomicU8>,
    voice_mode_flag: Arc<AtomicBool>,
    context_budget_flag: Arc<AtomicUsize>,
    model_flag: Arc<Mutex<String>>,
    repeat_interrupt_flag: Arc<AtomicBool>,
}

impl CodexCliDriver {
    pub fn new(
        model: String,
        sandbox: Option<String>,
        extra_env: Option<HashMap<String, String>>,
        working_dir: Arc<Mutex<PathBuf>>,
        tx_events: UnboundedSender<RoutedEvent>,
    ) -> Self {
        Self {
            thread_id: None,
            sandbox,
            extra_env,
            working_dir,
            tx_events,
            child: None,
            interrupt_flag: Arc::new(AtomicBool::new(false)),
            effort_flag: Arc::new(AtomicU8::new(Effort::None.to_u8())),
            voice_mode_flag: Arc::new(AtomicBool::new(false)),
            context_budget_flag: Arc::new(AtomicUsize::new(DEFAULT_CONTEXT_BUDGET)),
            model_flag: Arc::new(Mutex::new(model)),
            repeat_interrupt_flag: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn adopt_flags(&mut self, flags: &SharedFlags) {
        self.interrupt_flag = Arc::clone(&flags.interrupt);
        self.effort_flag = Arc::clone(&flags.effort);
        self.voice_mode_flag = Arc::clone(&flags.voice_mode);
        self.context_budget_flag = Arc::clone(&flags.context_budget);
        self.model_flag = Arc::clone(&flags.model);
        self.repeat_interrupt_flag = Arc::clone(&flags.repeat_interrupt);
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

    pub fn thread_id(&self) -> Option<&str> {
        self.thread_id.as_deref()
    }

    pub fn set_thread_id(&mut self, thread_id: Option<String>) {
        self.thread_id = thread_id;
    }

    pub fn clear_session(&mut self) {
        self.thread_id = None;
    }

    pub fn reset(&mut self) {
        self.clear_session();
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
        if image.is_some() {
            let _ = self.tx_events.send(RoutedEvent::own(StreamEvent::Info {
                message: "Codex does not support image attachments; the image was not sent."
                    .to_owned(),
            }));
        }

        let working_dir = self
            .working_dir
            .lock()
            .map_err(|_| HarnessError::Tool("working directory lock is poisoned".to_owned()))?
            .clone();
        let model = self
            .model_flag
            .lock()
            .map_err(|_| HarnessError::Tool("model lock is poisoned".to_owned()))?
            .clone();
        let effort = Effort::load(&self.effort_flag);
        let _voice_mode = self.voice_mode_flag.load(Ordering::SeqCst);
        let args = build_args(
            text,
            self.thread_id.as_deref(),
            self.sandbox.as_deref(),
            Some(&model),
            effort,
        );
        let spawned = spawn_codex(&args, &working_dir, self.extra_env.as_ref())?;
        let mut lines = BufReader::new(spawned.stdout).lines();
        let mut stderr = spawned.stderr;
        self.child = Some(spawned.child);
        let stderr_task = tokio::spawn(async move {
            let mut bytes = Vec::new();
            let result = stderr.read_to_end(&mut bytes).await;
            (result, bytes)
        });

        let mut mapper = EventMapper::new();
        let mut terminal = false;
        loop {
            if self.interrupt_flag.load(Ordering::SeqCst) {
                self.kill_child().await;
                self.interrupt_flag.store(false, Ordering::SeqCst);
                let _ = self
                    .tx_events
                    .send(RoutedEvent::own(StreamEvent::Interrupted {
                        message: "Interrupted by user (Escape)".to_owned(),
                    }));
                let _ = stderr_task.await;
                return Ok(());
            }

            match tokio::time::timeout(INTERRUPT_POLL_INTERVAL, lines.next_line()).await {
                Ok(Ok(Some(line))) => {
                    let Some(event) = parse_event(&line) else {
                        continue;
                    };
                    if let CodexEvent::ThreadStarted(started) = &event {
                        self.thread_id = Some(started.thread_id.clone());
                    }
                    terminal = matches!(
                        &event,
                        CodexEvent::TurnCompleted(_) | CodexEvent::TurnFailed(_)
                    );
                    for mapped in mapper.map(event) {
                        let _ = self.tx_events.send(RoutedEvent::own(mapped));
                    }
                    if terminal {
                        break;
                    }
                }
                Ok(Ok(None)) => break,
                Ok(Err(error)) => {
                    self.kill_child().await;
                    let _ = stderr_task.await;
                    return Err(error.into());
                }
                Err(_) => {}
            }
        }

        let status = self.wait_child().await?;
        let stderr_bytes = match stderr_task.await {
            Ok((Ok(_), bytes)) => bytes,
            Ok((Err(error), _)) => return Err(error.into()),
            Err(error) => {
                return Err(HarnessError::Tool(format!(
                    "codex CLI stderr task failed: {error}"
                )));
            }
        };
        if !terminal {
            let stderr = String::from_utf8_lossy(&stderr_bytes).trim().to_owned();
            return Err(HarnessError::Tool(if stderr.is_empty() {
                format!("codex CLI exited with {status} before a terminal event")
            } else {
                format!("codex CLI exited with {status} before a terminal event: {stderr}")
            }));
        }
        Ok(())
    }

    async fn kill_child(&mut self) {
        if let Some(mut child) = self.child.take() {
            if let Err(error) = child.start_kill() {
                tracing::warn!("codex_cli: failed to kill child: {error}");
            }
            if let Err(error) = child.wait().await {
                tracing::warn!("codex_cli: failed to reap killed child: {error}");
            }
        }
    }

    async fn wait_child(&mut self) -> Result<std::process::ExitStatus> {
        let mut child = self
            .child
            .take()
            .ok_or_else(|| HarnessError::Tool("codex CLI child is missing".to_owned()))?;
        Ok(child.wait().await?)
    }

    pub async fn shutdown(&mut self) {
        self.kill_child().await;
    }
}
