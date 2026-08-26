//! Unit tests for `deepseek_custom::memory`, moved out of the production
//! module as part of the two-crate workspace split.

use std::path::Path;

use deepseek_custom::memory::MemoryStore;

#[test]
fn loads_project_memory_when_present() {
    // Hermetic: write a CLAUDE.md into a scratch directory rather than
    // relying on the test binary's own working directory, which cargo
    // sets to the crate directory in a workspace, not the repo root.
    let root = std::env::temp_dir().join(format!(
        "deepseek-custom-memory-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("CLAUDE.md"), "# Test project instructions\n").unwrap();

    let store = MemoryStore::load(&root);
    assert!(store.project_claude_md.is_some());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn missing_memory_is_none() {
    let store = MemoryStore::load(Path::new("/nonexistent/path"));
    assert!(store.project_claude_md.is_none());
    assert!(store.project_memory_md.is_none());
}

#[test]
fn formatting_includes_non_empty() {
    let store = MemoryStore {
        project_claude_md: Some("# Rules\n\nBe concise.".into()),
        project_memory_md: None,
        global_claude_md: None,
        global_memory_md: Some("Remember X".into()),
    };
    let fragment = store.to_system_prompt_fragment();
    assert!(fragment.contains("## Project Instructions"));
    assert!(fragment.contains("Be concise"));
    assert!(fragment.contains("## Global Memory"));
    assert!(fragment.contains("Remember X"));
    assert!(!fragment.contains("## Project Memory")); // None → excluded
}

#[test]
fn reload_reads_changed_project_memory_from_disk() {
    let root = super::scratch_dir("dsc-memory", "reload");
    let path = root.join("MEMORY.md");
    std::fs::write(&path, "before").unwrap();
    let store = MemoryStore::load(&root);

    std::fs::write(&path, "after").unwrap();
    let reloaded = store.reload(&root);

    assert_eq!(reloaded.project_memory_md.as_deref(), Some("after"));
    let _ = std::fs::remove_dir_all(root);
}
