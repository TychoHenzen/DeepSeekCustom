use std::path::Path;

use tracing::{debug, info};

/// Stores project and global memory files (CLAUDE.md, MEMORY.md).
pub struct MemoryStore {
    pub project_claude_md: Option<String>,
    pub project_memory_md: Option<String>,
    pub global_claude_md: Option<String>,
    pub global_memory_md: Option<String>,
}

impl MemoryStore {
    /// Load all memory files from disk.
    pub fn load(project_root: &Path) -> Self {
        let store = Self {
            project_claude_md: Self::read_optional(&project_root.join("CLAUDE.md")),
            project_memory_md: Self::read_optional(&project_root.join("MEMORY.md")),
            global_claude_md: Self::read_global(".claude", "CLAUDE.md"),
            global_memory_md: Self::read_global(".claude", "MEMORY.md"),
        };

        info!(
            "memory: loaded project_claude={}, project_memory={}, global_claude={}, global_memory={}",
            store.project_claude_md.is_some(),
            store.project_memory_md.is_some(),
            store.global_claude_md.is_some(),
            store.global_memory_md.is_some(),
        );

        store
    }

    /// Format all memory content into a string for the system prompt.
    pub fn to_system_prompt_fragment(&self) -> String {
        let mut parts: Vec<String> = Vec::new();

        if let Some(ref content) = self.project_claude_md {
            parts.push(format!("## Project Instructions\n\n{content}"));
        }

        if let Some(ref content) = self.project_memory_md {
            parts.push(format!("## Project Memory\n\n{content}"));
        }

        if let Some(ref content) = self.global_claude_md {
            parts.push(format!("## Global Instructions\n\n{content}"));
        }

        if let Some(ref content) = self.global_memory_md {
            parts.push(format!("## Global Memory\n\n{content}"));
        }

        parts.join("\n\n")
    }

    // ── helpers ──

    fn read_optional(path: &Path) -> Option<String> {
        match std::fs::read_to_string(path) {
            Ok(content) => {
                if content.trim().is_empty() {
                    None
                } else {
                    debug!("memory: read {}", path.display());
                    Some(content)
                }
            }
            Err(_) => None,
        }
    }

    fn read_global(dir: &str, filename: &str) -> Option<String> {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .ok()?;
        Self::read_optional(&Path::new(&home).join(dir).join(filename))
    }
}
