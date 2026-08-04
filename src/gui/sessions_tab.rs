//! The Sessions tab: a "New Chat" button and a list of saved
//! conversations, each openable or deletable.
//!
//! The relative-time helper is plain logic over two Unix timestamps, kept
//! free of egui so it can be unit tested without a window.

use eframe::egui::{self, Color32, RichText};
use tracing::warn;

use crate::session::{SessionId, Timestamp, now_timestamp};

use super::{ActiveTab, DeepSeekGui};

/// Render a Unix timestamp `updated_at` as a short relative label against
/// `now`: "just now" under a minute, then minutes, hours, or days. A
/// timestamp in the future (clock skew between machines, or a save that
/// races the render) also reads "just now" rather than a negative or
/// nonsensical value.
pub(crate) fn relative_time_ago(now: Timestamp, updated_at: Timestamp) -> String {
    let elapsed_seconds = now.saturating_sub(updated_at);

    if elapsed_seconds < 60 {
        return "just now".to_string();
    }
    let elapsed_minutes = elapsed_seconds / 60;
    if elapsed_minutes < 60 {
        return format!("{elapsed_minutes}m ago");
    }
    let elapsed_hours = elapsed_minutes / 60;
    if elapsed_hours < 24 {
        return format!("{elapsed_hours}h ago");
    }
    let elapsed_days = elapsed_hours / 24;
    format!("{elapsed_days}d ago")
}

impl DeepSeekGui {
    /// Render the Sessions tab: a "New Chat" button, then one row per
    /// saved session (title, relative time, message count, a delete
    /// control), newest first. The currently open session is marked.
    pub(super) fn render_sessions_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Sessions");
        ui.separator();

        if ui.button("New Chat").clicked() {
            self.start_new_session();
            self.active_tab = ActiveTab::Chat;
        }

        ui.add_space(8.0);

        if self.saved_sessions.is_empty() {
            ui.label(
                RichText::new("No saved conversations yet.")
                    .color(Color32::GRAY)
                    .small(),
            );
            return;
        }

        let now = now_timestamp();
        let mut to_open: Option<SessionId> = None;
        let mut to_delete: Option<SessionId> = None;

        for meta in &self.saved_sessions {
            ui.horizontal(|ui| {
                let is_current = meta.id == self.current_session_id;
                let label = if is_current {
                    format!("* {}", meta.title)
                } else {
                    meta.title.clone()
                };
                if ui.link(label).clicked() {
                    to_open = Some(meta.id);
                }
                ui.label(
                    RichText::new(relative_time_ago(now, meta.updated_at))
                        .color(Color32::GRAY)
                        .small(),
                );
                ui.label(
                    RichText::new(format!("{} messages", meta.message_count))
                        .color(Color32::GRAY)
                        .small(),
                );
                if ui.small_button("Delete").clicked() {
                    to_delete = Some(meta.id);
                }
            });
        }

        if let Some(id) = to_open {
            self.load_session(id);
            self.active_tab = ActiveTab::Chat;
        }
        if let Some(id) = to_delete {
            self.delete_saved_session(id);
        }
    }

    /// Delete a saved session's file and refresh the list. Deleting the
    /// currently open session is allowed: it removes the file, and the
    /// open conversation stays as is, becoming unsaved again until the
    /// next autosave writes it back.
    fn delete_saved_session(&mut self, id: SessionId) {
        if let Err(e) = self.session_store.delete(&id) {
            warn!(error = %e, "failed to delete session");
        }
        self.saved_sessions = self.session_store.list();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_time_under_a_minute_is_just_now() {
        assert_eq!(relative_time_ago(1000, 970), "just now");
        assert_eq!(relative_time_ago(1000, 1000), "just now");
    }

    #[test]
    fn relative_time_in_minutes() {
        assert_eq!(relative_time_ago(1000 + 5 * 60, 1000), "5m ago");
        assert_eq!(relative_time_ago(1000 + 59 * 60, 1000), "59m ago");
    }

    #[test]
    fn relative_time_in_hours() {
        assert_eq!(relative_time_ago(1000 + 2 * 3600, 1000), "2h ago");
        assert_eq!(relative_time_ago(1000 + 23 * 3600, 1000), "23h ago");
    }

    #[test]
    fn relative_time_in_days() {
        assert_eq!(relative_time_ago(1000 + 3 * 86400, 1000), "3d ago");
    }

    #[test]
    fn relative_time_future_timestamp_does_not_panic_or_go_negative() {
        // Clock skew: `updated_at` is ahead of `now`. Must not panic and
        // must not render a negative or nonsensical value.
        let result = relative_time_ago(1000, 5000);
        assert_eq!(result, "just now");
    }
}
