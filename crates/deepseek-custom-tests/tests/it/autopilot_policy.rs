//! Unit tests for `deepseek_custom::autopilot::policy`, moved out of the
//! production module as part of the two-crate workspace split.

use std::path::PathBuf;

use deepseek_custom::autopilot::policy::{
    DEFAULT_POLICY_FILE, PolicyStore, format_policy_prompt_section,
};

/// Create a uniquely named directory under the system temp dir.
fn unique_temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("dsc-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn missing_policy_file_yields_empty_string() {
    let dir = unique_temp_dir("policy-missing");
    let store = PolicyStore::new(dir.clone(), None);
    assert_eq!(store.load_policy(), "");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn present_policy_file_is_read_back() {
    let dir = unique_temp_dir("policy-present");
    std::fs::write(dir.join(DEFAULT_POLICY_FILE), "Always ask twice.").unwrap();
    let store = PolicyStore::new(dir.clone(), None);
    assert_eq!(store.load_policy(), "Always ask twice.");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn relative_override_path_resolves_against_project_root() {
    let dir = unique_temp_dir("policy-relative");
    std::fs::write(dir.join("custom-policy.md"), "Custom rules.").unwrap();
    let store = PolicyStore::new(dir.clone(), Some("custom-policy.md".into()));
    assert_eq!(store.load_policy(), "Custom rules.");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn absolute_override_path_used_as_is() {
    let dir = unique_temp_dir("policy-absolute");
    let abs_path = dir.join("elsewhere-policy.md");
    std::fs::write(&abs_path, "Absolute rules.").unwrap();
    let store = PolicyStore::new(dir.clone(), Some(abs_path.to_string_lossy().to_string()));
    assert_eq!(store.load_policy(), "Absolute rules.");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn append_then_read_back_decision() {
    let dir = unique_temp_dir("decisions-append");
    let store = PolicyStore::new(dir.clone(), None);
    store.append_decision("What color?", "Blue");
    let recent = store.recent_decisions(10);
    assert_eq!(recent.len(), 1);
    assert!(recent[0].contains("What color?"));
    assert!(recent[0].contains("Blue"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn recent_entry_limit_returns_only_newest_n() {
    let dir = unique_temp_dir("decisions-limit");
    let store = PolicyStore::new(dir.clone(), None);
    for i in 0..5 {
        store.append_decision(&format!("Question {i}"), &format!("Answer {i}"));
    }
    let recent = store.recent_decisions(2);
    assert_eq!(recent.len(), 2);
    assert!(recent[0].contains("Question 3"));
    assert!(recent[1].contains("Question 4"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn missing_decision_log_yields_empty_list() {
    let dir = unique_temp_dir("decisions-missing");
    let store = PolicyStore::new(dir.clone(), None);
    assert!(store.recent_decisions(10).is_empty());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn newlines_in_question_or_answer_do_not_break_one_line_format() {
    let dir = unique_temp_dir("decisions-newlines");
    let store = PolicyStore::new(dir.clone(), None);
    store.append_decision("Multi\nline\nquestion", "Multi\nline\nanswer");
    let recent = store.recent_decisions(10);
    assert_eq!(recent.len(), 1);
    assert!(!recent[0].contains('\n'));
    assert!(recent[0].contains("Multi line question"));
    assert!(recent[0].contains("Multi line answer"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn prompt_formatter_empty_when_no_policy_and_no_decisions() {
    assert_eq!(format_policy_prompt_section("", &[]), "");
}

#[test]
fn prompt_formatter_includes_policy_and_decisions() {
    let decisions = vec!["question=Q1 answer=A1".to_string()];
    let section = format_policy_prompt_section("Be concise.", &decisions);
    assert!(section.contains("## Autopilot Policy"));
    assert!(section.contains("Be concise."));
    assert!(section.contains("## Recent Decisions"));
    assert!(section.contains("question=Q1 answer=A1"));
}
