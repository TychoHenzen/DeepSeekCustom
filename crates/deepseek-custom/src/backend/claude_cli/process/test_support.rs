//! Test-support seams for `ClaudeCliDriver`, gated behind the
//! `test-support` feature. Extracted from `mod.rs`.

use tokio::sync::mpsc;

use crate::error::Result;

use super::ClaudeCliDriver;

impl ClaudeCliDriver {
    /// Test seam for `ensure_ready`.
    #[cfg(feature = "test-support")]
    pub async fn ensure_ready_for_test(&mut self) -> Result<()> {
        self.ensure_ready().await
    }

    /// Test seam for `child_exited`.
    #[cfg(feature = "test-support")]
    pub fn child_exited_for_test(&mut self) -> bool {
        self.child_exited()
    }

    /// Test seam for `await_turn_end`.
    #[cfg(feature = "test-support")]
    pub async fn await_turn_end_for_test(&mut self) {
        self.await_turn_end().await
    }

    /// Test seam over the private `stdin` field.
    #[cfg(feature = "test-support")]
    pub fn stdin_is_none_for_test(&self) -> bool {
        self.stdin.is_none()
    }

    /// Test seam over the private `turn_done` field.
    #[cfg(feature = "test-support")]
    pub fn turn_done_is_none_for_test(&self) -> bool {
        self.turn_done.is_none()
    }

    /// Test seam to install a `turn_done` receiver directly.
    #[cfg(feature = "test-support")]
    pub fn set_turn_done_for_test(&mut self, rx: mpsc::UnboundedReceiver<()>) {
        self.turn_done = Some(rx);
    }

    /// Test seam to check the `turn_done` channel is empty after a signal
    /// was consumed.
    #[cfg(feature = "test-support")]
    pub fn turn_done_try_recv_is_err_for_test(&mut self) -> bool {
        self.turn_done
            .as_mut()
            .expect("turn_done must be set before calling this")
            .try_recv()
            .is_err()
    }

    /// Test seam over the private `spawned_working_dir` field.
    #[cfg(feature = "test-support")]
    pub fn spawned_working_dir_is_none_for_test(&self) -> bool {
        self.spawned_working_dir.is_none()
    }
}
