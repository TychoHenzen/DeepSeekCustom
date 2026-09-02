//! Tests for Codex CLI argument and process configuration assembly.

use std::path::{Path, PathBuf};

use deepseek_custom::backend::codex_cli::spawn::{
    build_args_for_test, build_planning_args_for_test, command_working_dir_for_test,
};
use deepseek_custom::effort::Effort;

#[test]
fn fresh_args_use_json_bypass_model_effort_and_final_prompt() {
    let args = build_args_for_test(
        "explain this",
        None,
        None,
        Some("gpt-5-codex"),
        Effort::High,
    );

    assert_eq!(
        args,
        vec![
            "exec",
            "--json",
            "--skip-git-repo-check",
            "--dangerously-bypass-approvals-and-sandbox",
            "-m",
            "gpt-5-codex",
            "-c",
            "reasoning.effort=high",
            "explain this",
        ]
    );
}

#[test]
fn explicit_sandbox_replaces_bypass() {
    let args = build_args_for_test("prompt", None, Some("workspace-write"), None, Effort::None);

    assert_eq!(
        args,
        vec![
            "exec",
            "--json",
            "--skip-git-repo-check",
            "--sandbox",
            "workspace-write",
            "prompt"
        ]
    );
    assert!(!args.iter().any(|arg| arg.contains("bypass")));
}

#[test]
fn none_omits_effort_override() {
    let args = build_args_for_test("prompt", None, None, None, Effort::None);

    assert!(!args.iter().any(|arg| arg == "-c"));
}

#[test]
fn every_supported_effort_uses_a_split_override_value() {
    for (effort, level) in [
        (Effort::Low, "low"),
        (Effort::Medium, "medium"),
        (Effort::High, "high"),
        (Effort::Max, "max"),
    ] {
        let args = build_args_for_test("prompt", None, None, None, effort);
        let index = args.iter().position(|arg| arg == "-c").unwrap();
        assert_eq!(args[index + 1], format!("reasoning.effort={level}"));
    }
}

#[test]
fn resume_args_put_identity_after_exec_and_prompt_last() {
    let args = build_args_for_test(
        "continue here",
        Some("thread-42"),
        Some("read-only"),
        Some("model-override"),
        Effort::Low,
    );

    assert_eq!(&args[..3], ["exec", "resume", "thread-42"]);
    assert!(args.windows(2).any(|pair| pair == ["-m", "model-override"]));
    assert_eq!(args.last().unwrap(), "continue here");
}

#[test]
fn spawn_command_uses_current_dir_without_a_dash_c_argument() {
    let working_dir = PathBuf::from("C:/projects/supplied-working-directory");
    let args = build_args_for_test("prompt", None, None, None, Effort::None);

    assert_eq!(
        command_working_dir_for_test(&args, &working_dir),
        Some(working_dir)
    );
    assert!(!args.iter().any(|arg| arg == "-C"));
    assert_eq!(
        command_working_dir_for_test(&args, Path::new("C:/another-directory")),
        Some(PathBuf::from("C:/another-directory"))
    );
}

#[test]
fn controlled_planning_args_force_fresh_read_only_isolated_structured_output() {
    let schema = PathBuf::from(r"C:\Temp\controlled-work-card-schema.json");
    let args = build_planning_args_for_test(
        "produce one card",
        &schema,
        Some("gpt-5-codex"),
        Effort::High,
    );

    assert_eq!(
        args,
        vec![
            "exec",
            "--json",
            "--skip-git-repo-check",
            "--sandbox",
            "read-only",
            "--ephemeral",
            "--ignore-user-config",
            "--ignore-rules",
            "--output-schema",
            r"C:\Temp\controlled-work-card-schema.json",
            "-m",
            "gpt-5-codex",
            "-c",
            "reasoning.effort=high",
            "produce one card",
        ]
    );
    assert!(!args.iter().any(|argument| argument == "resume"));
    assert!(!args.iter().any(|argument| argument.contains("bypass")));
}
