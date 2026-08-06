//! Unit tests for `deepseek_custom::hemisphere`, moved out of the production
//! module as part of the two-crate workspace split.

use deepseek_custom::agent::history::MessageHistory;
use deepseek_custom::api::types::Message;
use deepseek_custom::hemisphere::{HemisphereConfig, compress_for_right, fits_context_window};

#[test]
fn hemisphere_config_defaults_disabled() {
    let cfg = HemisphereConfig::default();
    assert!(!cfg.enabled);
    assert_eq!(cfg.right_context_window, 4000);
    assert_eq!(cfg.right_max_response, 200);
}

#[test]
fn compress_empty_history() {
    let h = MessageHistory::new("system".into());
    let result = compress_for_right(&h, 3);
    assert!(result.contains("empty conversation"));
}

#[test]
fn compress_reduces_history() {
    let mut h = MessageHistory::new("system".into());
    for i in 0..10 {
        h.push(Message::user(format!("message {i}")));
    }
    let result = compress_for_right(&h, 3);
    assert!(result.contains("Summary of earlier conversation"));
    assert!(result.contains("message 9")); // last verbatim
    assert!(!result.contains("message 0")); // summarized away
}

#[test]
fn fits_context_detects_overflow() {
    let small = "short";
    assert!(fits_context_window(small, 100));

    let large = "x".repeat(10000);
    assert!(!fits_context_window(&large, 100));
}
