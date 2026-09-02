use std::path::{Path, PathBuf};

use deepseek_custom::procedure::{
    DisposableDraftWorkspace, DisposableWorkspaceError, DisposableWorkspaceOptions,
    PromotionBaseline, PromotionTarget, SnapshotProgress, WorkspaceFileRename,
    promote_verified_workspace,
};

fn temp_dir(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "dsc disposable source {tag} {}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn write(root: &Path, relative: &str, contents: impl AsRef<[u8]>) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

#[test]
fn snapshot_copies_nested_source_with_spaces_and_excludes_generated_and_binary_outputs() {
    let source = temp_dir("copy");
    write(&source, "src/nested file.rs", "pub fn copied() {}\n");
    write(&source, ".git/config", "excluded");
    write(&source, "target/debug/output.txt", "excluded");
    write(&source, ".deepseek/report.json", "excluded");
    write(&source, "bin/program.EXE", "excluded by extension");
    write(&source, "assets/raw.dat", b"text\0binary");
    write(&source, "assets/invalid.dat", [0xff, 0xfe]);

    let workspace = DisposableDraftWorkspace::create(&source).unwrap();

    assert_eq!(
        std::fs::read_to_string(workspace.path().join("src/nested file.rs")).unwrap(),
        "pub fn copied() {}\n"
    );
    for excluded in [
        ".git",
        "target",
        ".deepseek",
        "bin/program.EXE",
        "assets/raw.dat",
        "assets/invalid.dat",
    ] {
        assert!(
            !workspace.path().join(excluded).exists(),
            "unexpected copied path: {excluded}"
        );
    }

    std::fs::remove_dir_all(source).ok();
}

#[test]
fn current_state_snapshot_copies_tracked_untracked_and_uncommitted_source_bytes() {
    let source = temp_dir("current state");
    write(&source, "src/tracked.rs", "initial source\n");
    write(&source, "src/untracked.rs", "untracked source\n");
    std::fs::write(
        source.join("src/tracked.rs"),
        "current uncommitted source\n",
    )
    .unwrap();
    write(&source, "assets/source.bin", [0x00, 0xff, 0x10, 0x80]);

    let workspace = DisposableDraftWorkspace::create_current_state(&source).unwrap();

    assert_eq!(
        std::fs::read(workspace.path().join("src/tracked.rs")).unwrap(),
        b"current uncommitted source\n"
    );
    assert_eq!(
        std::fs::read(workspace.path().join("src/untracked.rs")).unwrap(),
        b"untracked source\n"
    );
    assert_eq!(
        std::fs::read(workspace.path().join("assets/source.bin")).unwrap(),
        [0x00, 0xff, 0x10, 0x80]
    );

    std::fs::remove_dir_all(source).ok();
}

#[test]
fn snapshot_never_copies_or_traverses_a_directory_link() {
    let source = temp_dir("link root");
    let outside = temp_dir("link target");
    write(&source, "kept.txt", "kept");
    write(&outside, "escaped.txt", "outside");
    let link = source.join("linked");
    let file_link = source.join("linked-file");
    assert!(
        create_directory_link(&outside, &link),
        "test platform must support a directory link or junction"
    );
    let file_link_created = create_file_link(&outside.join("escaped.txt"), &file_link);

    let workspace = DisposableDraftWorkspace::create(&source).unwrap();

    assert_eq!(
        std::fs::read_to_string(workspace.path().join("kept.txt")).unwrap(),
        "kept"
    );
    assert!(!workspace.path().join("linked").exists());
    if file_link_created {
        assert!(!workspace.path().join("linked-file").exists());
    }

    std::fs::remove_dir(&link).ok();
    std::fs::remove_file(&file_link).ok();
    std::fs::remove_dir_all(source).ok();
    std::fs::remove_dir_all(outside).ok();
}

#[test]
fn configured_output_trees_are_excluded_by_relative_prefix() {
    let source = temp_dir("configured exclusions");
    write(&source, "src/kept.rs", "kept\n");
    write(&source, "dist/bundle.js", "generated\n");
    write(&source, "reports/coverage/index.html", "generated\n");
    write(&source, "reports/kept.txt", "kept\n");
    let options = DisposableWorkspaceOptions {
        excluded_paths: vec!["dist".into(), "reports/coverage".into()],
        max_bytes: DisposableWorkspaceOptions::default().max_bytes,
    };

    let workspace =
        DisposableDraftWorkspace::create_current_state_with_options(&source, &options).unwrap();

    assert!(workspace.path().join("src/kept.rs").is_file());
    assert!(!workspace.path().join("dist").exists());
    assert!(!workspace.path().join("reports/coverage").exists());
    assert!(workspace.path().join("reports/kept.txt").is_file());

    std::fs::remove_dir_all(source).ok();
}

#[test]
fn snapshot_rejects_oversized_input_before_creating_workspace() {
    let source = temp_dir("size limit");
    write(&source, "src/large.rs", "0123456789");
    let options = DisposableWorkspaceOptions {
        excluded_paths: Vec::new(),
        max_bytes: 5,
    };

    let error =
        DisposableDraftWorkspace::create_current_state_with_options(&source, &options).unwrap_err();

    assert!(matches!(
        error,
        DisposableWorkspaceError::SnapshotTooLarge {
            max_bytes: 5,
            required_bytes: 10
        }
    ));
    std::fs::remove_dir_all(source).ok();
}

#[test]
fn snapshot_reports_copy_progress_and_recovery_retention_is_explicit() {
    let source = temp_dir("progress and recovery");
    write(&source, "a.txt", "one");
    write(&source, "nested/b.txt", "two");
    let options = DisposableWorkspaceOptions::default();
    let mut progress = Vec::<SnapshotProgress>::new();

    let workspace = DisposableDraftWorkspace::create_current_state_with_progress(
        &source,
        &options,
        &mut |event| progress.push(event),
    )
    .unwrap();
    assert_eq!(progress.first().unwrap().bytes_copied, 0);
    assert_eq!(progress.last().unwrap().files_copied, 2);
    assert_eq!(progress.last().unwrap().bytes_copied, 6);
    let recovery = workspace.retain_for_recovery();
    let recovery_path = recovery.path().to_path_buf();
    assert!(recovery_path.is_dir());
    recovery.cleanup().unwrap();
    assert!(!recovery_path.exists());

    std::fs::remove_dir_all(source).ok();
}

#[test]
fn explicit_close_and_drop_both_remove_the_snapshot() {
    let source = temp_dir("cleanup");
    write(&source, "src/lib.rs", "pub fn source() {}\n");

    let explicit = DisposableDraftWorkspace::create(&source).unwrap();
    let explicit_path = explicit.path().to_path_buf();
    explicit.close().unwrap();
    assert!(!explicit_path.exists());

    let dropped_path = {
        let dropped = DisposableDraftWorkspace::create(&source).unwrap();
        dropped.path().to_path_buf()
    };
    assert!(!dropped_path.exists());

    std::fs::remove_dir_all(source).ok();
}

// covers: deepseek-custom/controlled-development-mode :: Execution uses one isolated current-state workspace :: Passing packet starts from current dirty bytes
#[test]
fn execution_pair_forks_current_dirty_bytes_and_reports_deterministic_endpoints() {
    let source = temp_dir("controlled execution pair");
    write(&source, "src/approved.rs", "before\n");
    write(
        &source,
        "src/unrelated-dirty.rs",
        "dirty before execution\n",
    );
    write(&source, "src/deleted.rs", "delete me\n");
    write(&source, "src/renamed.rs", "rename me\n");
    write(&source, ".git/config", "excluded metadata\n");
    write(&source, "target/output.txt", "excluded build output\n");
    write(&source, ".deepseek/run.json", "excluded harness data\n");
    write(&source, "dist/output.js", "configured output\n");

    let pair = DisposableDraftWorkspace::create_current_state_pair_with_options(
        &source,
        &DisposableWorkspaceOptions {
            excluded_paths: vec!["dist".into()],
            ..DisposableWorkspaceOptions::default()
        },
    )
    .unwrap();

    for root in [pair.baseline_path(), pair.execution_path()] {
        assert_eq!(
            std::fs::read(root.join("src/unrelated-dirty.rs")).unwrap(),
            b"dirty before execution\n"
        );
        assert!(!root.join(".git").exists());
        assert!(!root.join("target").exists());
        assert!(!root.join(".deepseek").exists());
        assert!(!root.join("dist").exists());
    }
    assert!(
        std::fs::write(pair.baseline_path().join("src/approved.rs"), "forbidden\n").is_err(),
        "the execution baseline must be immutable"
    );

    write(pair.execution_path(), "src/approved.rs", "after\n");
    write(pair.execution_path(), "src/created.rs", "created\n");
    write(
        pair.execution_path(),
        "dist/generated.js",
        "ignored output\n",
    );
    std::fs::remove_file(pair.execution_path().join("src/deleted.rs")).unwrap();
    std::fs::rename(
        pair.execution_path().join("src/renamed.rs"),
        pair.execution_path().join("src/moved.rs"),
    )
    .unwrap();

    let changes = pair.changes().unwrap();
    assert_eq!(changes.created, ["src/created.rs"]);
    assert_eq!(changes.modified, ["src/approved.rs"]);
    assert_eq!(changes.deleted, ["src/deleted.rs"]);
    assert_eq!(
        changes.renamed,
        [WorkspaceFileRename {
            from: "src/renamed.rs".into(),
            to: "src/moved.rs".into(),
        }]
    );
    assert_eq!(
        changes.changed_paths(),
        [
            "src/approved.rs",
            "src/created.rs",
            "src/deleted.rs",
            "src/moved.rs",
            "src/renamed.rs",
        ]
    );
    assert_eq!(changes, pair.changes().unwrap());

    let targets = [PromotionTarget::Update {
        path: "src/approved.rs".into(),
    }];
    let baseline = PromotionBaseline::capture(&source, &targets).unwrap();
    write(
        &source,
        "src/unrelated-dirty.rs",
        "user changed unrelated bytes later\n",
    );
    promote_verified_workspace(&source, pair.execution_path(), &baseline, &targets).unwrap();
    assert_eq!(
        std::fs::read(source.join("src/approved.rs")).unwrap(),
        b"after\n"
    );
    assert_eq!(
        std::fs::read(source.join("src/unrelated-dirty.rs")).unwrap(),
        b"user changed unrelated bytes later\n"
    );
    assert!(!source.join("src/created.rs").exists());

    let retained = pair.retain_for_diagnostics();
    let baseline_path = retained.baseline_path().to_path_buf();
    let execution_path = retained.execution_path().to_path_buf();
    retained.cleanup().unwrap();
    assert!(!baseline_path.exists());
    assert!(!execution_path.exists());
    std::fs::remove_dir_all(source).ok();
}

#[cfg(windows)]
fn create_directory_link(target: &Path, link: &Path) -> bool {
    if std::os::windows::fs::symlink_dir(target, link).is_ok() {
        return true;
    }
    std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .is_ok_and(|output| output.status.success())
}

#[cfg(windows)]
fn create_file_link(target: &Path, link: &Path) -> bool {
    std::os::windows::fs::symlink_file(target, link).is_ok()
}

#[cfg(unix)]
fn create_file_link(target: &Path, link: &Path) -> bool {
    std::os::unix::fs::symlink(target, link).is_ok()
}

#[cfg(not(any(windows, unix)))]
fn create_file_link(_target: &Path, _link: &Path) -> bool {
    false
}

#[cfg(unix)]
fn create_directory_link(target: &Path, link: &Path) -> bool {
    std::os::unix::fs::symlink(target, link).is_ok()
}

#[cfg(not(any(windows, unix)))]
fn create_directory_link(_target: &Path, _link: &Path) -> bool {
    false
}
