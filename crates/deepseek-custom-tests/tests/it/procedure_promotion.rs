use std::path::{Path, PathBuf};

use deepseek_custom::procedure::{
    PromotionBaseline, PromotionBaselineCheckError, PromotionFailureInjection,
    PromotionRecoveryEvidence, PromotionTarget, PromotionTargetKind, capture_path_fingerprints,
    decode_patch_envelope, model_promotion_targets, promote_verified_workspace,
    promote_verified_workspace_with_failure_injection, validate_patch_boundary,
};

const ALL_ENDPOINTS_DIFF: &str = concat!(
    "diff --git a/src/updated.rs b/src/updated.rs\n",
    "--- a/src/updated.rs\n",
    "+++ b/src/updated.rs\n",
    "@@ -1 +1 @@\n",
    "-before\n",
    "+after\n",
    "diff --git a/src/created.rs b/src/created.rs\n",
    "new file mode 100644\n",
    "--- /dev/null\n",
    "+++ b/src/created.rs\n",
    "@@ -0,0 +1 @@\n",
    "+created\n",
    "diff --git a/src/deleted.rs b/src/deleted.rs\n",
    "deleted file mode 100644\n",
    "--- a/src/deleted.rs\n",
    "+++ /dev/null\n",
    "@@ -1 +0,0 @@\n",
    "-deleted\n",
    "diff --git a/src/old.rs b/src/new.rs\n",
    "similarity index 100%\n",
    "rename from src/old.rs\n",
    "rename to src/new.rs\n",
);

fn temp_dir(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("dsc promotion {tag} {}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn all_endpoint_targets() -> Vec<PromotionTarget> {
    let encoded = serde_json::json!({
        "targets": [
            "src/created.rs",
            "src/deleted.rs",
            "src/new.rs",
            "src/old.rs",
            "src/updated.rs"
        ],
        "rationale": "Model every promotion endpoint.",
        "route": {
            "automatic_tier": "local",
            "effective_tier": "local",
            "selected_override": "automatic",
            "overridden": false,
            "signals": [{"kind": "target_count", "value": 5}]
        },
        "unified_diff": ALL_ENDPOINTS_DIFF
    });
    let candidate = decode_patch_envelope(&encoded.to_string()).unwrap();
    let patch = validate_patch_boundary(
        candidate,
        [
            "src/created.rs",
            "src/deleted.rs",
            "src/new.rs",
            "src/old.rs",
            "src/updated.rs",
        ],
    )
    .unwrap();
    model_promotion_targets(&patch).unwrap()
}

fn source_with_all_baselines(tag: &str) -> PathBuf {
    let root = temp_dir(tag);
    write(&root, "src/updated.rs", "before\n");
    write(&root, "src/deleted.rs", "deleted\n");
    write(&root, "src/old.rs", "renamed\n");
    root
}

#[test]
fn promotion_targets_model_create_update_delete_and_rename_endpoints() {
    let targets = all_endpoint_targets();

    assert_eq!(targets.len(), 4);
    assert_eq!(targets[0].kind(), PromotionTargetKind::Update);
    assert_eq!(targets[1].kind(), PromotionTargetKind::Create);
    assert_eq!(targets[2].kind(), PromotionTargetKind::Delete);
    assert_eq!(targets[3].kind(), PromotionTargetKind::Rename);
    assert_eq!(targets[0].paths(), ["src/updated.rs"]);
    assert_eq!(targets[1].paths(), ["src/created.rs"]);
    assert_eq!(targets[2].paths(), ["src/deleted.rs"]);
    assert_eq!(targets[3].paths(), ["src/old.rs", "src/new.rs"]);
}

#[test]
fn matching_preview_baseline_allows_promotion_for_every_endpoint() {
    let root = source_with_all_baselines("matching");
    let targets = all_endpoint_targets();
    let baseline = PromotionBaseline::capture(&root, &targets).unwrap();

    let comparison = baseline.ensure_current(&root).unwrap();

    assert!(comparison.can_promote());
    assert!(comparison.stale_paths.is_empty());
    assert_eq!(
        comparison.checked_paths,
        [
            "src/created.rs",
            "src/deleted.rs",
            "src/new.rs",
            "src/old.rs",
            "src/updated.rs"
        ]
    );
    assert_eq!(
        baseline
            .fingerprints()
            .iter()
            .map(|fingerprint| fingerprint.state)
            .collect::<Vec<_>>(),
        vec![
            deepseek_custom::procedure::ProcedurePathState::Missing,
            deepseek_custom::procedure::ProcedurePathState::Present,
            deepseek_custom::procedure::ProcedurePathState::Missing,
            deepseek_custom::procedure::ProcedurePathState::Present,
            deepseek_custom::procedure::ProcedurePathState::Present,
        ]
    );

    std::fs::remove_dir_all(root).ok();
}

#[test]
fn concurrent_edits_report_every_stale_real_endpoint_and_block_promotion() {
    let root = source_with_all_baselines("stale");
    let targets = all_endpoint_targets();
    let baseline = PromotionBaseline::capture(&root, &targets).unwrap();

    write(&root, "src/created.rs", "created by another edit\n");
    write(&root, "src/updated.rs", "changed by another edit\n");
    std::fs::remove_file(root.join("src/deleted.rs")).unwrap();
    std::fs::remove_file(root.join("src/old.rs")).unwrap();
    write(&root, "src/new.rs", "created by another edit\n");

    let comparison = baseline.compare(&root).unwrap();
    assert!(!comparison.can_promote());
    assert_eq!(
        comparison
            .stale_paths
            .iter()
            .map(|stale| stale.path.as_str())
            .collect::<Vec<_>>(),
        [
            "src/created.rs",
            "src/deleted.rs",
            "src/new.rs",
            "src/old.rs",
            "src/updated.rs"
        ]
    );
    assert!(
        comparison
            .stale_paths
            .iter()
            .all(|stale| stale.expected != stale.actual)
    );

    let error = baseline.ensure_current(&root).unwrap_err();
    assert!(matches!(
        error,
        PromotionBaselineCheckError::Stale { stale_paths } if stale_paths.len() == 5
    ));

    std::fs::remove_dir_all(root).ok();
}

#[test]
fn successful_promotion_installs_all_endpoint_results_and_removes_recovery_files() {
    let root = source_with_all_baselines("successful transaction");
    let verified = temp_dir("verified transaction result");
    write(&verified, "src/updated.rs", "after\n");
    write(&verified, "src/created.rs", "created\n");
    write(&verified, "src/new.rs", "renamed\n");

    let targets = all_endpoint_targets();
    let baseline = PromotionBaseline::capture(&root, &targets).unwrap();
    let result = promote_verified_workspace(&root, &verified, &baseline, &targets).unwrap();

    assert!(result.baseline.can_promote());
    assert_eq!(
        std::fs::read_to_string(root.join("src/updated.rs")).unwrap(),
        "after\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("src/created.rs")).unwrap(),
        "created\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("src/new.rs")).unwrap(),
        "renamed\n"
    );
    assert!(!root.join("src/deleted.rs").exists());
    assert!(!root.join("src/old.rs").exists());

    let final_fingerprints = capture_path_fingerprints(
        &root,
        [
            "src/created.rs".to_string(),
            "src/deleted.rs".to_string(),
            "src/new.rs".to_string(),
            "src/old.rs".to_string(),
            "src/updated.rs".to_string(),
        ],
    )
    .unwrap();
    assert_eq!(result.final_fingerprints, final_fingerprints);
    let leftovers = std::fs::read_dir(root.join("src"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains("deepseek-promotion"))
        .collect::<Vec<_>>();
    assert!(leftovers.is_empty(), "promotion leftovers: {leftovers:?}");

    std::fs::remove_dir_all(root).ok();
    std::fs::remove_dir_all(verified).ok();
}

fn update_and_create_targets() -> Vec<PromotionTarget> {
    vec![
        PromotionTarget::Update {
            path: "src/updated.rs".to_string(),
        },
        PromotionTarget::Create {
            path: "src/created.rs".to_string(),
        },
    ]
}

fn assert_complete_rollback(error: deepseek_custom::procedure::PromotionError) {
    let deepseek_custom::procedure::PromotionError::Transaction { recovery, .. } = error else {
        panic!("mid-promotion failure must report a transaction error");
    };
    assert_complete_recovery(&recovery);
}

fn assert_complete_recovery(recovery: &PromotionRecoveryEvidence) {
    assert!(
        recovery.rollback_succeeded(),
        "recovery evidence: {recovery:?}"
    );
    assert!(!recovery.requires_recovery());
    assert!(recovery.rollback_errors.is_empty());
    assert!(recovery.recovery_paths.is_empty());
}

#[test]
fn injected_mid_promotion_failure_restores_existing_and_removes_created_paths() {
    let root = temp_dir("injected install failure");
    let verified = temp_dir("injected install result");
    write(&root, "src/updated.rs", "before\n");
    write(&verified, "src/updated.rs", "after\n");
    write(&verified, "src/created.rs", "created\n");
    let targets = update_and_create_targets();
    let baseline = PromotionBaseline::capture(&root, &targets).unwrap();

    let error = promote_verified_workspace_with_failure_injection(
        &root,
        &verified,
        &baseline,
        &targets,
        PromotionFailureInjection {
            fail_install_at: Some(1),
            fail_rollback_at: None,
        },
    )
    .unwrap_err();
    assert_complete_rollback(error);

    assert_eq!(
        std::fs::read_to_string(root.join("src/updated.rs")).unwrap(),
        "before\n"
    );
    assert!(!root.join("src/created.rs").exists());
    let leftovers = std::fs::read_dir(root.join("src"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains("deepseek-promotion"))
        .collect::<Vec<_>>();
    assert!(leftovers.is_empty(), "promotion leftovers: {leftovers:?}");

    std::fs::remove_dir_all(root).ok();
    std::fs::remove_dir_all(verified).ok();
}

#[test]
fn injected_rollback_failure_retains_recovery_data_and_reports_it() {
    let root = temp_dir("injected rollback failure");
    let verified = temp_dir("injected rollback result");
    write(&root, "src/updated.rs", "before\n");
    write(&verified, "src/updated.rs", "after\n");
    write(&verified, "src/created.rs", "created\n");
    let targets = update_and_create_targets();
    let baseline = PromotionBaseline::capture(&root, &targets).unwrap();

    let error = promote_verified_workspace_with_failure_injection(
        &root,
        &verified,
        &baseline,
        &targets,
        PromotionFailureInjection {
            fail_install_at: Some(1),
            fail_rollback_at: Some(1),
        },
    )
    .unwrap_err();
    let deepseek_custom::procedure::PromotionError::Transaction { recovery, .. } = error else {
        panic!("mid-promotion failure must report a transaction error");
    };
    assert!(!recovery.rollback_succeeded());
    assert!(recovery.requires_recovery());
    assert!(!recovery.rollback_errors.is_empty());
    assert!(recovery.recovery_paths.iter().all(|path| path.exists()));
    assert!(
        recovery
            .recovery_paths
            .iter()
            .any(|path| path.to_string_lossy().contains("updated.rs"))
    );
    assert!(!root.join("src/created.rs").exists());

    std::fs::remove_dir_all(root).ok();
    std::fs::remove_dir_all(verified).ok();
}
