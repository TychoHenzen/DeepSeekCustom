//! `RepeatTarget` implementation for `CodexCliDriver`.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::agent::events::{RoutedEvent, StreamEvent};
use crate::agent::repeat::RepeatTarget;
use crate::error::Result;

use super::CodexCliDriver;

impl CodexCliDriver {
    /// Publish one `StreamEvent` on this driver's event channel.
    fn send_event(&self, event: StreamEvent) {
        let _ = self.tx_events.send(RoutedEvent::own(event));
    }
}

impl RepeatTarget for CodexCliDriver {
    async fn reset_for_iteration(&mut self) {
        self.clear_session();
    }

    async fn run_turn(&mut self, task: &str) -> Result<String> {
        self.send(task).await?;
        Ok(String::new())
    }

    fn repeat_interrupt_flag(&self) -> Arc<AtomicBool> {
        self.repeat_interrupt_flag()
    }

    fn send_event(&self, event: StreamEvent) {
        self.send_event(event)
    }
}
