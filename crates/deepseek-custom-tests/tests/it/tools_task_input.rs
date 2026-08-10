//! Tests for `deepseek_custom::tools::task::input` (`build_request`,
//! `TaskInput`). Split from `tools_task.rs` when `task.rs` became a
//! directory module.

use std::path::PathBuf;

use deepseek_custom::effort::Effort;
use deepseek_custom::tools::task::build_request;

fn well_formed_input() -> serde_json::Value {
    serde_json::json!({
        "description": "check the weather",
        "prompt": "What is 2 + 2?",
        "backend": "nope",
    })
}

#[test]
fn build_request_at_dispatch_depth_one_carries_depth_through() {
    let request = build_request(well_formed_input(), 1, Effort::None).expect("should parse");
    assert_eq!(request.depth, 1);
    assert_eq!(request.backend, "nope");
    assert_eq!(request.prompt, "What is 2 + 2?");
    assert_eq!(request.model, None);
    assert!(!request.keep_open);
}

#[test]
fn build_request_defaults_keep_open_to_false_when_absent() {
    let request = build_request(well_formed_input(), 1, Effort::None).expect("should parse");
    assert!(!request.keep_open);
}

#[test]
fn build_request_honors_an_explicit_keep_open_true() {
    let input = serde_json::json!({
        "description": "check the weather",
        "prompt": "What is 2 + 2?",
        "backend": "nope",
        "keep_open": true,
    });
    let request = build_request(input, 1, Effort::None).expect("should parse");
    assert!(request.keep_open);
}

#[test]
fn build_request_defaults_working_dir_override_to_none_when_absent() {
    let request = build_request(well_formed_input(), 1, Effort::None).expect("should parse");
    assert_eq!(request.working_dir_override, None);
}

#[test]
fn build_request_honors_an_explicit_working_dir() {
    let input = serde_json::json!({
        "description": "check the weather",
        "prompt": "What is 2 + 2?",
        "backend": "nope",
        "working_dir": "C:/sibling-checkout",
    });
    let request = build_request(input, 1, Effort::None).expect("should parse");
    assert_eq!(
        request.working_dir_override,
        Some(PathBuf::from("C:/sibling-checkout"))
    );
}

#[test]
fn build_request_missing_prompt_is_an_error_not_a_panic() {
    let input = serde_json::json!({
        "description": "check the weather",
        "backend": "nope",
    });
    let err = match build_request(input, 1, Effort::None) {
        Ok(_) => panic!("missing prompt should fail to parse"),
        Err(e) => e,
    };
    assert!(err.contains("Invalid Task input"));
}

#[test]
fn build_request_falls_back_to_the_given_default_effort_when_absent() {
    let request = build_request(well_formed_input(), 1, Effort::Max).expect("should parse");
    assert_eq!(request.effort, Effort::Max);
}

#[test]
fn build_request_honors_an_explicit_effort_override_over_the_default() {
    let input = serde_json::json!({
        "description": "check the weather",
        "prompt": "What is 2 + 2?",
        "backend": "nope",
        "effort": "high",
    });
    let request = build_request(input, 1, Effort::Low).expect("should parse");
    assert_eq!(request.effort, Effort::High);
}

#[test]
fn build_request_with_an_unrecognised_effort_value_is_an_error_not_a_panic() {
    let input = serde_json::json!({
        "description": "check the weather",
        "prompt": "What is 2 + 2?",
        "backend": "nope",
        "effort": "extreme",
    });
    let err = match build_request(input, 1, Effort::None) {
        Ok(_) => panic!("an unrecognised effort value should fail to parse"),
        Err(e) => e,
    };
    assert!(err.contains("Invalid Task input"));
}
