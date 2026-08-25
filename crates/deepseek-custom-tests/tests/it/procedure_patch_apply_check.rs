use std::path::{Path, PathBuf};

use deepseek_custom::procedure::{
    PatchApplyCheckError, check_patch_applicability, decode_patch_envelope, validate_patch_boundary,
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
