use std::path::{Path, PathBuf};

use deepseek_custom::procedure::DisposableDraftWorkspace;

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
    assert!(
        create_directory_link(&outside, &link),
        "test platform must support a directory link or junction"
    );

    let workspace = DisposableDraftWorkspace::create(&source).unwrap();

    assert_eq!(
        std::fs::read_to_string(workspace.path().join("kept.txt")).unwrap(),
        "kept"
    );
    assert!(!workspace.path().join("linked").exists());

    std::fs::remove_dir(&link).ok();
    std::fs::remove_dir_all(source).ok();
    std::fs::remove_dir_all(outside).ok();
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

#[cfg(unix)]
fn create_directory_link(target: &Path, link: &Path) -> bool {
    std::os::unix::fs::symlink(target, link).is_ok()
}

#[cfg(not(any(windows, unix)))]
fn create_directory_link(_target: &Path, _link: &Path) -> bool {
    false
}
