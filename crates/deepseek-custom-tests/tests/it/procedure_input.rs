use std::path::{Path, PathBuf};

use deepseek_custom::procedure::{ContractSelection, OpenSpecInput, OpenSpecInputError};

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "dsc-procedure-input-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_fake_openspec(dir: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let path = dir.join("fake-openspec.cmd");
        std::fs::write(
            &path,
            "@echo off\r\necho %*\r\nif \"%2\"==\"valid-change\" exit /b 0\r\necho exact validation failure for %2 1>&2\r\nexit /b 17\r\n",
        )
        .unwrap();
        path
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;

        let path = dir.join("fake-openspec");
        std::fs::write(
            &path,
            "#!/bin/sh\nprintf '%s\\n' \"$*\"\nif [ \"$2\" = \"valid-change\" ]; then exit 0; fi\nprintf 'exact validation failure for %s\\n' \"$2\" >&2\nexit 17\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        path
    }
}

fn write_change(root: &Path, change_id: &str, tasks: &str, capability: &str, spec: &str) {
    let change = root.join("openspec/changes").join(change_id);
    let spec_dir = change.join("specs").join(capability);
    std::fs::create_dir_all(&spec_dir).unwrap();
    std::fs::write(change.join("tasks.md"), tasks).unwrap();
    std::fs::write(
        change.join("proposal.md"),
        "# Proposal\n\n## Why\n\nKeep the selected change small.\n\n## What Changes\n\n- Add the selected behavior.\n\n## Impact\n\nUNRELATED_PROPOSAL_SENTINEL\n",
    )
    .unwrap();
    std::fs::write(spec_dir.join("spec.md"), spec).unwrap();
}

fn bound_spec() -> &'static str {
    "## Purpose\n\nBound fixture.\n\n## ADDED Requirements\n\n### Requirement: Selected requirement\nThe selected requirement text.\n\n#### Scenario: Selected scenario\n- **WHEN** selected input arrives\n- **THEN** selected output is produced\n\n#### Scenario: Unrelated scenario\nUNRELATED_SCENARIO_SENTINEL\n\n### Requirement: Unrelated requirement\nUNRELATED_REQUIREMENT_SENTINEL\n\n#### Scenario: Other\n- **THEN** nothing selected\n"
}

fn write_unbound_change_with_capability_count(root: &Path, change_id: &str, count: usize) {
    let change = root.join("openspec/changes").join(change_id);
    std::fs::create_dir_all(&change).unwrap();
    std::fs::write(change.join("tasks.md"), "- [ ] 1.1 Unbound task\n").unwrap();
    std::fs::write(
        change.join("proposal.md"),
        "# Proposal\n\n## Why\n\nSelect one contract.\n\n## What Changes\n\n- Exercise contract selection.\n",
    )
    .unwrap();
    for index in 0..count {
        let spec_dir = change.join("specs").join(format!("capability-{index}"));
        std::fs::create_dir_all(&spec_dir).unwrap();
        std::fs::write(spec_dir.join("spec.md"), bound_spec()).unwrap();
    }
}

#[test]
fn strict_validation_uses_the_required_argument_order() {
    let root = temp_dir("valid");
    let command = write_fake_openspec(&root);
    let input = OpenSpecInput::with_command(&root, command.display().to_string());

    let result = input.validate_change("valid-change").unwrap();

    assert_eq!(result.exit_code, Some(0));
    assert_eq!(
        result.stdout.trim(),
        "validate valid-change --strict --no-interactive"
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn invalid_change_preserves_exit_code_stdout_and_exact_stderr() {
    let root = temp_dir("invalid");
    let command = write_fake_openspec(&root);
    let input = OpenSpecInput::with_command(&root, command.display().to_string());

    let error = input.validate_change("invalid-change").unwrap_err();

    match error {
        OpenSpecInputError::ValidationFailed(failure) => {
            assert_eq!(failure.exit_code, Some(17));
            assert_eq!(
                failure.stdout.trim(),
                "validate invalid-change --strict --no-interactive"
            );
            assert_eq!(
                failure.stderr,
                if cfg!(windows) {
                    "exact validation failure for invalid-change \r\n"
                } else {
                    "exact validation failure for invalid-change\n"
                }
            );
        }
        other => panic!("expected validation failure, got {other:?}"),
    }
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn missing_change_keeps_the_cli_failure_instead_of_reimplementing_validation() {
    let root = temp_dir("missing-change");
    let command = write_fake_openspec(&root);
    let input = OpenSpecInput::with_command(&root, command.display().to_string());

    let error = input.validate_change("missing-change").unwrap_err();

    assert!(matches!(
        error,
        OpenSpecInputError::ValidationFailed(ref failure)
            if failure.stderr.contains("exact validation failure for missing-change")
    ));
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn missing_executable_preserves_the_spawn_error() {
    let root = temp_dir("missing-command");
    let missing = root.join("definitely-missing-openspec.exe");
    let input = OpenSpecInput::with_command(&root, missing.display().to_string());

    let error = input.validate_change("any-change").unwrap_err();
    let message = error.to_string();

    assert!(matches!(error, OpenSpecInputError::CommandSpawn { .. }));
    assert!(message.contains("definitely-missing-openspec.exe"));
    assert!(message.contains(&root.display().to_string()));
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn active_changes_are_sorted_and_expose_only_unchecked_tasks() {
    let root = temp_dir("active");
    write_change(
        &root,
        "z-change",
        "- [x] 1.1 Completed task\n- [ ] 1.2 Pending task\n  <!-- covers: sample/capability :: Selected requirement :: Selected scenario -->\n",
        "sample/capability",
        bound_spec(),
    );
    write_change(
        &root,
        "a-change",
        "- [ ] 2.1 First pending task\n",
        "sample/capability",
        bound_spec(),
    );
    write_change(
        &root,
        "archive/old-change",
        "- [ ] 9.9 Archived task\n",
        "sample/capability",
        bound_spec(),
    );

    let changes = OpenSpecInput::new(&root).active_changes().unwrap();

    assert_eq!(
        changes
            .iter()
            .map(|change| change.id.as_str())
            .collect::<Vec<_>>(),
        vec!["a-change", "z-change"]
    );
    assert_eq!(changes[1].tasks.len(), 1);
    assert_eq!(changes[1].tasks[0].id, "1.2");
    assert_eq!(
        changes[1].tasks[0].covers.as_deref(),
        Some("sample/capability :: Selected requirement :: Selected scenario")
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn a_bound_task_loads_only_its_named_requirement_and_scenario() {
    let root = temp_dir("bound");
    let command = write_fake_openspec(&root);
    write_change(
        &root,
        "valid-change",
        "- [ ] 2.1 Bound task\n  <!-- covers: sample/capability :: Selected requirement :: Selected scenario -->\n",
        "sample/capability",
        bound_spec(),
    );
    let input = OpenSpecInput::with_command(&root, command.display().to_string());

    let selected = input
        .validate_and_select_task("valid-change", "2.1")
        .unwrap();

    assert_eq!(selected.validation.exit_code, Some(0));
    assert_eq!(
        selected.contract.proposal_scope.why,
        "Keep the selected change small."
    );
    match selected.contract.selection {
        ContractSelection::Bound {
            capability,
            requirement,
        } => {
            assert_eq!(capability, "sample/capability");
            assert_eq!(requirement.name, "Selected requirement");
            assert_eq!(requirement.scenarios.len(), 1);
            assert_eq!(requirement.scenarios[0].name, "Selected scenario");
            let json = serde_json::to_string(&requirement).unwrap();
            assert!(!json.contains("UNRELATED_SCENARIO_SENTINEL"));
            assert!(!json.contains("UNRELATED_REQUIREMENT_SENTINEL"));
        }
        other => panic!("expected bound contract, got {other:?}"),
    }
    std::fs::remove_dir_all(root).ok();
}

// covers: deepseek-custom/procedure-localization :: Task contract selection is unambiguous :: One capability supports an unbound task
#[test]
fn one_capability_change_selects_the_complete_delta_for_an_unbound_task() {
    let root = temp_dir("unbound");
    let command = write_fake_openspec(&root);
    write_change(
        &root,
        "valid-change",
        "- [ ] 2.2 Unbound task text\n",
        "sample/capability",
        bound_spec(),
    );
    let input = OpenSpecInput::with_command(&root, command.display().to_string());

    let selected = input
        .validate_and_select_task("valid-change", "2.2")
        .unwrap();

    assert_eq!(selected.contract.task.text, "Unbound task text");
    assert_eq!(
        selected.contract.proposal_scope.what_changes,
        "- Add the selected behavior."
    );
    let encoded_scope = serde_json::to_string(&selected.contract.proposal_scope).unwrap();
    assert!(!encoded_scope.contains("UNRELATED_PROPOSAL_SENTINEL"));
    match selected.contract.selection {
        ContractSelection::Unbound { capability_delta } => {
            assert_eq!(capability_delta.capability, "sample/capability");
            assert_eq!(capability_delta.purpose, "Bound fixture.");
            assert_eq!(capability_delta.requirements.len(), 2);
            assert_eq!(
                capability_delta.requirements[0].name,
                "Selected requirement"
            );
            assert_eq!(
                capability_delta.requirements[0].text,
                "The selected requirement text."
            );
            assert_eq!(capability_delta.requirements[0].scenarios.len(), 2);
            assert_eq!(
                capability_delta.requirements[0].scenarios[0].name,
                "Selected scenario"
            );
            assert_eq!(
                capability_delta.requirements[0].scenarios[0].text,
                "- **WHEN** selected input arrives\n- **THEN** selected output is produced"
            );
            assert_eq!(
                capability_delta.requirements[0].scenarios[1].name,
                "Unrelated scenario"
            );
            assert_eq!(
                capability_delta.requirements[0].scenarios[1].text,
                "UNRELATED_SCENARIO_SENTINEL"
            );
            assert_eq!(
                capability_delta.requirements[1].name,
                "Unrelated requirement"
            );
            assert_eq!(
                capability_delta.requirements[1].text,
                "UNRELATED_REQUIREMENT_SENTINEL"
            );
            assert_eq!(capability_delta.requirements[1].scenarios.len(), 1);
            assert_eq!(capability_delta.requirements[1].scenarios[0].name, "Other");
            assert_eq!(
                capability_delta.requirements[1].scenarios[0].text,
                "- **THEN** nothing selected"
            );
        }
        other => panic!("expected unbound contract, got {other:?}"),
    }
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn an_unbound_task_with_zero_capability_deltas_reports_the_complete_error() {
    let root = temp_dir("unbound-zero-capabilities");
    let command = write_fake_openspec(&root);
    write_unbound_change_with_capability_count(&root, "valid-change", 0);
    let input = OpenSpecInput::with_command(&root, command.display().to_string());

    let error = input
        .validate_and_select_task("valid-change", "1.1")
        .unwrap_err();

    assert_eq!(
        error.to_string(),
        "OpenSpec input error: unbound task `1.1` in change `valid-change` needs exactly one capability delta, found 0"
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn an_unbound_task_with_multiple_capability_deltas_reports_the_complete_error() {
    let root = temp_dir("unbound-multiple-capabilities");
    let command = write_fake_openspec(&root);
    write_unbound_change_with_capability_count(&root, "valid-change", 2);
    let input = OpenSpecInput::with_command(&root, command.display().to_string());

    let error = input
        .validate_and_select_task("valid-change", "1.1")
        .unwrap_err();

    assert_eq!(
        error.to_string(),
        "OpenSpec input error: unbound task `1.1` in change `valid-change` needs exactly one capability delta, found 2"
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn a_completed_task_is_rejected_as_a_selection() {
    let root = temp_dir("completed");
    let command = write_fake_openspec(&root);
    write_change(
        &root,
        "valid-change",
        "- [x] 1.1 Completed task\n",
        "sample/capability",
        bound_spec(),
    );
    let input = OpenSpecInput::with_command(&root, command.display().to_string());

    let error = input
        .validate_and_select_task("valid-change", "1.1")
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("is completed and cannot be selected")
    );
    std::fs::remove_dir_all(root).ok();
}
