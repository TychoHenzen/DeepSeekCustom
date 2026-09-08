//! `RepeatTarget` implementation for `ClaudeCliDriver`. Extracted from
//! `mod.rs`.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::agent::events::{RoutedEvent, StreamEvent};
use crate::agent::repeat::RepeatTarget;
use crate::error::Result;

use super::ClaudeCliDriver;

impl ClaudeCliDriver {
    /// Publish one `StreamEvent` on this driver's event channel.
    pub(super) fn send_event(&self, event: StreamEvent) {
        let _ = self.tx_events.send(RoutedEvent::own(event));
    }
}

impl RepeatTarget for ClaudeCliDriver {
    /// End the current child so the next turn spawns a fresh one.
    async fn reset_for_iteration(&mut self) {
        self.shutdown().await;
    }

    async fn run_turn(&mut self, task: &str) -> Result<String> {
        self.send(task).await?;
        let reply = self
            .last_reply
            .lock()
            .map(|r| r.clone())
            .unwrap_or_default();
        Ok(reply)
    }

    fn repeat_interrupt_flag(&self) -> Arc<AtomicBool> {
        self.repeat_interrupt_flag()
    }

    fn send_event(&self, event: StreamEvent) {
        self.send_event(event)
    }
}
