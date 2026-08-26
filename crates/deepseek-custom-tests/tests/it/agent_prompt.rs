//! Unit tests for `deepseek_custom::agent::prompt`, moved out of the
//! production module as part of the two-crate workspace split.

use deepseek_custom::agent::prompt::{build_system_prompt, voice_mode_instructions};
use deepseek_custom::api::types::{FunctionDef, ToolDef};

#[test]
fn system_prompt_includes_all_sections() {
    let tools = vec![ToolDef {
        tool_type: "function".into(),
        function: FunctionDef {
            name: "read".into(),
            description: "Read a file".into(),
            parameters: serde_json::json!({}),
        },
    }];
    let prompt = build_system_prompt(Some("memory content"), Some("skill list"), &tools);

    assert!(prompt.contains("memory content"));
    assert!(prompt.contains("skill list"));
    assert!(prompt.contains("\"name\": \"read\""));
}

#[test]
fn system_prompt_handles_empty_optionals() {
    let prompt = build_system_prompt(None, None, &[]);

    assert!(prompt.contains("DeepSeekCustom"));
    assert!(!prompt.contains("## Project Context"));
    assert!(!prompt.contains("## Available Skills"));
    assert!(!prompt.contains("## Available Tools"));
}

#[test]
fn voice_mode_instructions_is_non_empty() {
    assert!(!voice_mode_instructions().is_empty());
}

#[test]
fn voice_mode_instructions_has_heading() {
    assert!(voice_mode_instructions().contains("## Voice reply mode"));
}

#[test]
fn voice_mode_instructions_mentions_sentence_cap() {
    assert!(voice_mode_instructions().contains("two sentences"));
}
