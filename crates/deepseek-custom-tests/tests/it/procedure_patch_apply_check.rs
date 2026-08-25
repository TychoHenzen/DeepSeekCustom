use std::path::{Path, PathBuf};
use std::process::Command;

use deepseek_custom::procedure::{
    PatchApplyCheckError, apply_patch_in_workspace, check_patch_applicability,
    decode_patch_envelope, validate_patch_boundary,
};

fn temp_dir(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "dsc patch apply check {tag} {}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

fn init_repository(root: &Path) {
    let output = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn boundary_patch(diff: &str) -> deepseek_custom::procedure::BoundaryValidatedPatch {
    let encoded = serde_json::json!({
        "route": {
            "automatic_tier": "frontier",
            "effective_tier": "frontier",
            "selected_override": "automatic",
            "overridden": false,
            "signals": [{"kind": "substantive_logic"}]
        },
        "targets": ["src/lib.rs"],
        "rationale": "Validate this patch against a disposable snapshot.",
        "unified_diff": diff,
    })
    .to_string();
    let candidate = decode_patch_envelope(&encoded).unwrap();
    validate_patch_boundary(candidate, ["src/lib.rs"]).unwrap()
}

#[test]
fn matching_hunks_are_checked_without_applying_them_to_the_source() {
    let source = temp_dir("matching source with spaces");
    write(&source, "src/lib.rs", "pub fn old() {}\n");
    let before = std::fs::read(source.join("src/lib.rs")).unwrap();
    let patch = boundary_patch(
        "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-pub fn old() {}\n+pub fn new() {}\n",
    );

    let checked = check_patch_applicability(&source, patch).unwrap();

    assert_eq!(checked.boundary_patch().paths(), ["src/lib.rs"]);
    assert_eq!(std::fs::read(source.join("src/lib.rs")).unwrap(), before);
    std::fs::remove_dir_all(source).ok();
}

#[test]
fn context_mismatch_is_rejected_without_changing_the_source() {
    let source = temp_dir("mismatched source");
    write(&source, "src/lib.rs", "pub fn actual() {}\n");
    let before = std::fs::read(source.join("src/lib.rs")).unwrap();
    let patch = boundary_patch(
        "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-pub fn expected() {}\n+pub fn replacement() {}\n",
    );

    let error = check_patch_applicability(&source, patch).unwrap_err();

    assert!(matches!(error, PatchApplyCheckError::Rejected { .. }));
    assert!(
        error
            .to_string()
            .contains("patch hunks do not apply to the current source snapshot")
    );
    assert_eq!(std::fs::read(source.join("src/lib.rs")).unwrap(), before);
    std::fs::remove_dir_all(source).ok();
}

#[test]
fn valid_patch_is_checked_and_applied_only_in_the_verification_workspace() {
    let source = temp_dir("valid temporary repository");
    init_repository(&source);
    write(&source, "src/lib.rs", "pub fn old() {}\n");
    let before = std::fs::read(source.join("src/lib.rs")).unwrap();
    let patch = boundary_patch(
        "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-pub fn old() {}\n+pub fn new() {}\n",
    );

    let applied = apply_patch_in_workspace(&source, patch).unwrap();
    let workspace_path = applied.path().to_path_buf();

    assert_eq!(std::fs::read(source.join("src/lib.rs")).unwrap(), before);
    assert_eq!(
        std::fs::read_to_string(applied.path().join("src/lib.rs")).unwrap(),
        "pub fn new() {}\n"
    );
    assert!(applied.check_result().success);
    assert_eq!(applied.check_result().status_code, Some(0));
    assert!(applied.apply_result().success);
    assert_eq!(applied.apply_result().status_code, Some(0));
    assert_eq!(
        applied.check_result().phase,
        deepseek_custom::procedure::GitApplyPhase::Check
    );
    assert_eq!(
        applied.apply_result().phase,
        deepseek_custom::procedure::GitApplyPhase::Apply
    );

    drop(applied);
    assert!(!workspace_path.exists());
    std::fs::remove_dir_all(source).ok();
}

#[test]
fn invalid_patch_is_rejected_before_apply_and_real_repository_stays_unchanged() {
    let source = temp_dir("invalid temporary repository");
    init_repository(&source);
    write(&source, "src/lib.rs", "pub fn actual() {}\n");
    let before = std::fs::read(source.join("src/lib.rs")).unwrap();
    let patch = boundary_patch(
        "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-pub fn expected() {}\n+pub fn replacement() {}\n",
    );

    let error = apply_patch_in_workspace(&source, patch).unwrap_err();

    assert!(matches!(error, PatchApplyCheckError::Rejected { .. }));
    assert_eq!(std::fs::read(source.join("src/lib.rs")).unwrap(), before);
    std::fs::remove_dir_all(source).ok();
}
