//! Unit tests for `deepseek_custom::gui::sessions_tab` (`src/gui/sessions_tab.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use deepseek_custom::gui::sessions_tab::relative_time_ago;

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
