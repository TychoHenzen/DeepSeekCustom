//! Unit tests for `deepseek_custom::voice::cuda_dlls` (`src/voice/cuda_dlls.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use deepseek_custom::voice::cuda_dlls::{
    cuda_toolkit_bin_dirs_under, nvidia_bin_dirs_under, toolkit_bin_dir,
};

/// A directory unique to this test run under the OS temp dir, cleaned
/// up by the returned guard on drop.
struct TempTree {
    root: PathBuf,
}

impl TempTree {
    fn new(label: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        // Rooted directly under the OS temp dir, not under this crate's own
        // package directory: the package directory moved two levels down
        // earlier in the workspace split and broke path-relative tests at
        // the time. `std::env::temp_dir()` is unaffected by that move.
        let root = std::env::temp_dir().join(format!("cuda_dlls_test_{label}_{nanos}_{n}"));
        fs::create_dir_all(&root).expect("create temp tree root");
        Self { root }
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn nvidia_bin_dirs_under_finds_bin_dirs_for_each_component() {
    let tree = TempTree::new("found");
    fs::create_dir_all(tree.root.join("nvidia/cudnn/bin")).unwrap();
    fs::create_dir_all(tree.root.join("nvidia/cublas/bin")).unwrap();
    // A component with no bin/ subdirectory must not appear in results.
    fs::create_dir_all(tree.root.join("nvidia/no_bin_here/lib")).unwrap();

    let mut found = nvidia_bin_dirs_under(std::slice::from_ref(&tree.root));
    found.sort();

    let mut expected = vec![
        tree.root.join("nvidia/cudnn/bin"),
        tree.root.join("nvidia/cublas/bin"),
    ];
    expected.sort();
    assert_eq!(found, expected);
}

#[test]
fn nvidia_bin_dirs_under_returns_empty_when_no_nvidia_dir_present() {
    let tree = TempTree::new("no_nvidia");
    // tree.root exists but has no `nvidia` child at all.
    let found = nvidia_bin_dirs_under(std::slice::from_ref(&tree.root));
    assert!(found.is_empty());
}

#[test]
fn nvidia_bin_dirs_under_returns_empty_for_nonexistent_site_packages() {
    let missing = PathBuf::from("Z:/no/such/site-packages/anywhere");
    let found = nvidia_bin_dirs_under(&[missing]);
    assert!(found.is_empty());
}

#[test]
fn nvidia_bin_dirs_under_empty_input_returns_empty() {
    let found = nvidia_bin_dirs_under(&[]);
    assert!(found.is_empty());
}

#[test]
fn nvidia_bin_dirs_under_merges_results_from_multiple_site_packages() {
    let tree_a = TempTree::new("multi_a");
    let tree_b = TempTree::new("multi_b");
    fs::create_dir_all(tree_a.root.join("nvidia/cudnn/bin")).unwrap();
    fs::create_dir_all(tree_b.root.join("nvidia/cufft/bin")).unwrap();

    let mut found = nvidia_bin_dirs_under(&[tree_a.root.clone(), tree_b.root.clone()]);
    found.sort();

    let mut expected = vec![
        tree_a.root.join("nvidia/cudnn/bin"),
        tree_b.root.join("nvidia/cufft/bin"),
    ];
    expected.sort();
    assert_eq!(found, expected);
}

#[test]
fn toolkit_bin_dir_prefers_bin_x64_when_present() {
    let tree = TempTree::new("toolkit_x64");
    let version_dir = tree.root.join("v13.0");
    fs::create_dir_all(version_dir.join("bin/x64")).unwrap();
    assert_eq!(
        toolkit_bin_dir(&version_dir),
        Some(version_dir.join("bin/x64"))
    );
}

#[test]
fn toolkit_bin_dir_falls_back_to_bin_when_no_x64() {
    let tree = TempTree::new("toolkit_plain");
    let version_dir = tree.root.join("v12.6");
    fs::create_dir_all(version_dir.join("bin")).unwrap();
    assert_eq!(toolkit_bin_dir(&version_dir), Some(version_dir.join("bin")));
}

#[test]
fn toolkit_bin_dir_none_when_neither_exists() {
    let tree = TempTree::new("toolkit_missing");
    let version_dir = tree.root.join("v11.0");
    assert_eq!(toolkit_bin_dir(&version_dir), None);
}

#[test]
fn cuda_toolkit_bin_dirs_under_finds_every_version() {
    let tree = TempTree::new("toolkit_multi");
    let cuda_root = tree.root.join("CUDA");
    fs::create_dir_all(cuda_root.join("v12.6/bin")).unwrap();
    fs::create_dir_all(cuda_root.join("v13.0/bin/x64")).unwrap();

    let mut found = cuda_toolkit_bin_dirs_under(&cuda_root);
    found.sort();

    let mut expected = vec![cuda_root.join("v12.6/bin"), cuda_root.join("v13.0/bin/x64")];
    expected.sort();
    assert_eq!(found, expected);
}

#[test]
fn cuda_toolkit_bin_dirs_under_returns_empty_when_root_missing() {
    let missing = PathBuf::from("Z:/no/such/CUDA/root");
    assert!(cuda_toolkit_bin_dirs_under(&missing).is_empty());
}
