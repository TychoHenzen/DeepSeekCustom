use std::path::{Path, PathBuf};

use deepseek_custom::config::settings::RepositoryIndexLimits;
use deepseek_custom::procedure::{
    LocalizationTarget, RepositoryIndexEntry, build_repository_index, validate_localization_targets,
};

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "dsc-procedure-index-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn generous_limits() -> RepositoryIndexLimits {
    RepositoryIndexLimits {
        max_files: 100,
        max_total_bytes: 1_000_000,
    }
}

fn write(root: &Path, relative: &str, contents: impl AsRef<[u8]>) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

#[test]
fn repository_index_paths_are_relative_normalized_and_sorted() {
    let root = temp_dir("sorted");
    write(&root, "zeta.txt", "z");
    write(&root, "nested/alpha.txt", "a");

    let index = build_repository_index(&root, &generous_limits()).unwrap();
    let paths: Vec<&str> = index.iter().map(|entry| entry.path.as_str()).collect();

    assert_eq!(paths, vec!["nested/alpha.txt", "zeta.txt"]);
    assert!(paths.iter().all(|path| !path.contains('\\')));
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn repository_index_excludes_generated_trees_links_and_binary_files() {
    let root = temp_dir("excluded");
    write(&root, "kept.txt", "kept");
    write(&root, ".git/config", "hidden");
    write(&root, "target/output.txt", "hidden");
    write(&root, ".deepseek/report.json", "hidden");
    write(&root, "image.png", b"not really an image");
    write(&root, "nul.dat", b"text\0binary");

    let index = build_repository_index(&root, &generous_limits()).unwrap();
    let paths: Vec<&str> = index.iter().map(|entry| entry.path.as_str()).collect();

    assert_eq!(paths, vec!["kept.txt"]);
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn repository_index_reports_a_missing_root() {
    let root = temp_dir("missing");
    std::fs::remove_dir_all(&root).unwrap();

    let error = build_repository_index(&root, &generous_limits()).unwrap_err();

    assert!(
        error.to_string().contains("root is not a directory"),
        "{error}"
    );
}

#[test]
fn rust_paths_include_conservative_declared_symbols() {
    let root = temp_dir("rust-symbols");
    write(
        &root,
        "src/items.rs",
        r#"
pub(crate) struct Visible;
pub enum Choice { A }
async unsafe fn do_work() {}
pub extern "C" fn on_wire() {}
const LIMIT: usize = 1;
macro_rules! make_item { () => {} }
"#,
    );

    let index = build_repository_index(&root, &generous_limits()).unwrap();

    assert_eq!(index.len(), 1);
    assert_eq!(
        index[0].symbols,
        vec![
            "Choice",
            "LIMIT",
            "Visible",
            "do_work",
            "make_item",
            "on_wire"
        ]
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn comments_and_strings_do_not_create_rust_symbols() {
    let root = temp_dir("masked-symbols");
    write(
        &root,
        "src/items.rs",
        r##"
// fn commented_out() {}
/* pub struct BlockComment; */
const TEXT: &str = r#"
fn inside_raw_string() {}
"#;
fn real_item() {}
"##,
    );

    let index = build_repository_index(&root, &generous_limits()).unwrap();

    assert_eq!(index[0].symbols, vec!["TEXT", "real_item"]);
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn non_rust_paths_remain_available_without_symbols() {
    let root = temp_dir("path-only");
    write(&root, "docs/guide.md", "fn prose_example() {}");

    let index = build_repository_index(&root, &generous_limits()).unwrap();

    assert_eq!(index[0].path, "docs/guide.md");
    assert!(index[0].symbols.is_empty());
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn valid_localization_targets_are_returned_unchanged() {
    let index = vec![RepositoryIndexEntry {
        path: "src/lib.rs".to_string(),
        symbols: vec!["run".to_string()],
    }];
    let targets = vec![
        LocalizationTarget {
            path: "src/lib.rs".to_string(),
            symbol: Some("run".to_string()),
            evidence: "entry point".to_string(),
        },
        LocalizationTarget {
            path: "src/lib.rs".to_string(),
            symbol: None,
            evidence: "whole file".to_string(),
        },
    ];

    let accepted = validate_localization_targets(targets.clone(), &index).unwrap();

    assert_eq!(accepted, targets);
}

#[test]
fn invalid_paths_and_symbols_are_reported_together() {
    let index = vec![RepositoryIndexEntry {
        path: "src/lib.rs".to_string(),
        symbols: vec!["run".to_string()],
    }];
    let targets = vec![
        LocalizationTarget {
            path: "src/missing.rs".to_string(),
            symbol: None,
            evidence: "invented path".to_string(),
        },
        LocalizationTarget {
            path: "src/lib.rs".to_string(),
            symbol: Some("invented_symbol".to_string()),
            evidence: "invented symbol".to_string(),
        },
    ];

    let error = validate_localization_targets(targets, &index).unwrap_err();
    let message = error.to_string();

    assert_eq!(error.rejections().len(), 2);
    assert!(message.contains("src/missing.rs"), "{message}");
    assert!(message.contains("path is not present"), "{message}");
    assert!(message.contains("invented_symbol"), "{message}");
    assert!(message.contains("symbol is not present"), "{message}");
}

#[test]
fn traversal_absolute_and_invented_targets_reject_the_whole_result() {
    let index = vec![RepositoryIndexEntry {
        path: "src/lib.rs".to_string(),
        symbols: vec!["run".to_string()],
    }];
    let targets = vec![
        LocalizationTarget {
            path: "src/lib.rs".to_string(),
            symbol: Some("run".to_string()),
            evidence: "valid target must not make a partial result valid".to_string(),
        },
        LocalizationTarget {
            path: "src/../src/lib.rs".to_string(),
            symbol: None,
            evidence: "traversal".to_string(),
        },
        LocalizationTarget {
            path: "C:/outside.rs".to_string(),
            symbol: None,
            evidence: "absolute".to_string(),
        },
        LocalizationTarget {
            path: "src/invented.rs".to_string(),
            symbol: None,
            evidence: "invented".to_string(),
        },
    ];

    let error = validate_localization_targets(targets, &index).unwrap_err();
    let message = error.to_string();

    assert_eq!(error.rejections().len(), 3);
    assert!(message.contains("src/../src/lib.rs"), "{message}");
    assert!(message.contains("traversal or dot components"), "{message}");
    assert!(message.contains("C:/outside.rs"), "{message}");
    assert!(message.contains("must be repository-relative"), "{message}");
    assert!(message.contains("src/invented.rs"), "{message}");
}

#[test]
fn unicode_repository_paths_remain_valid_targets() {
    let root = temp_dir("unicode");
    write(&root, "src/naive_λ.rs", "pub fn localise() {}\n");

    let index = build_repository_index(&root, &generous_limits()).unwrap();
    let target = LocalizationTarget {
        path: "src/naive_λ.rs".to_string(),
        symbol: Some("localise".to_string()),
        evidence: "Unicode path".to_string(),
    };

    assert_eq!(index[0].path, "src/naive_λ.rs");
    assert_eq!(
        validate_localization_targets(vec![target.clone()], &index).unwrap(),
        vec![target]
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn file_count_limit_failure_names_the_first_sorted_overflow() {
    let root = temp_dir("file-limit");
    write(&root, "b.txt", "b");
    write(&root, "a.txt", "a");
    let limits = RepositoryIndexLimits {
        max_files: 1,
        max_total_bytes: 100,
    };

    let error = build_repository_index(&root, &limits).unwrap_err();
    let message = error.to_string();

    assert!(
        message.contains("file limit exceeded at b.txt"),
        "{message}"
    );
    assert!(message.contains("max_files is 1"), "{message}");
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn total_byte_limit_failure_names_the_first_sorted_overflow() {
    let root = temp_dir("byte-limit");
    write(&root, "b.txt", "bbbb");
    write(&root, "a.txt", "aaa");
    let limits = RepositoryIndexLimits {
        max_files: 10,
        max_total_bytes: 5,
    };

    let error = build_repository_index(&root, &limits).unwrap_err();
    let message = error.to_string();

    assert!(
        message.contains("byte limit exceeded at b.txt"),
        "{message}"
    );
    assert!(message.contains("7 bytes"), "{message}");
    assert!(message.contains("max_total_bytes 5"), "{message}");
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn invalid_utf8_content_is_excluded_as_binary() {
    let root = temp_dir("invalid-utf8");
    write(&root, "kept.txt", "kept");
    write(&root, "binary.dat", [0xff, 0xfe, 0xfd]);

    let index = build_repository_index(&root, &generous_limits()).unwrap();

    assert_eq!(index.len(), 1);
    assert_eq!(index[0].path, "kept.txt");
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn oversized_binary_is_excluded_before_the_text_byte_limit_is_applied() {
    let root = temp_dir("oversized-binary");
    let mut binary = vec![b'a'; 9_000];
    binary.push(0);
    write(&root, "large.dat", binary);
    write(&root, "kept.txt", "kept");
    let limits = RepositoryIndexLimits {
        max_files: 10,
        max_total_bytes: 10,
    };

    let index = build_repository_index(&root, &limits).unwrap();

    assert_eq!(index.len(), 1);
    assert_eq!(index[0].path, "kept.txt");
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn directory_links_are_not_traversed() {
    let root = temp_dir("link-root");
    let outside = temp_dir("link-outside");
    write(&root, "kept.txt", "kept");
    write(&outside, "escaped.txt", "must not be indexed");
    let link = root.join("linked");
    assert!(
        create_directory_link(&outside, &link),
        "test platform must support a directory link or junction"
    );

    let index = build_repository_index(&root, &generous_limits()).unwrap();
    let paths: Vec<&str> = index.iter().map(|entry| entry.path.as_str()).collect();

    assert_eq!(paths, vec!["kept.txt"]);
    std::fs::remove_dir(&link).ok();
    std::fs::remove_dir_all(root).ok();
    std::fs::remove_dir_all(outside).ok();
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
