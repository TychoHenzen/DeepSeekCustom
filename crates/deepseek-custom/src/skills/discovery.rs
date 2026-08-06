//! Finding skill files on disk, across every root a skill can live under.
//!
//! Three roots, in the order a later one overrides an earlier one by name:
//! enabled plugins, then `~/.claude/skills/`, then `<project>/skills/`.
//! Within a root, two file shapes count. A flat `<name>.md`, and a
//! directory holding `SKILL.md`. Claude Code writes the second shape, and
//! the old loader here matched only the first, which is why 95 of the 99
//! skills on this machine were invisible to the agent.

use std::path::{Path, PathBuf};

use tracing::debug;

use crate::plugins::{PluginRoot, claude_home, enabled_plugin_roots};

/// Which root a skill was found under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillSource {
    /// `<project_root>/skills/`.
    Project,
    /// `~/.claude/skills/`.
    Global,
    /// `<plugin_root>/skills/`, named by the plugin it came from.
    Plugin(String),
}

impl SkillSource {
    /// A short label for a log line or a prompt index entry.
    pub fn label(&self) -> String {
        match self {
            SkillSource::Project => "project".to_string(),
            SkillSource::Global => "global".to_string(),
            SkillSource::Plugin(name) => format!("plugin:{name}"),
        }
    }
}

/// Every skill markdown file reachable from this project, tagged with the
/// root it came from. Ordered so that a later entry overrides an earlier
/// one of the same name: plugins first, then global, then project.
pub fn discover_skill_files(project_root: &Path) -> Vec<(PathBuf, SkillSource)> {
    let mut found = Vec::new();

    for plugin in enabled_plugin_roots() {
        collect_from_root(
            &plugin.root.join("skills"),
            &SkillSource::Plugin(plugin.name.clone()),
            &mut found,
        );
    }

    if let Some(home) = claude_home() {
        collect_from_root(&home.join("skills"), &SkillSource::Global, &mut found);
    }

    collect_from_root(
        &project_root.join("skills"),
        &SkillSource::Project,
        &mut found,
    );

    found
}

/// The same discovery against explicit roots, for a test that must not see
/// whatever the running machine happens to have installed.
pub fn discover_skill_files_in(
    project_root: &Path,
    global_dir: Option<&Path>,
    plugins: &[PluginRoot],
) -> Vec<(PathBuf, SkillSource)> {
    let mut found = Vec::new();
    for plugin in plugins {
        collect_from_root(
            &plugin.root.join("skills"),
            &SkillSource::Plugin(plugin.name.clone()),
            &mut found,
        );
    }
    if let Some(dir) = global_dir {
        collect_from_root(dir, &SkillSource::Global, &mut found);
    }
    collect_from_root(
        &project_root.join("skills"),
        &SkillSource::Project,
        &mut found,
    );
    found
}

/// Collect both skill file shapes from one `skills/` directory. A missing
/// or unreadable directory contributes nothing and is not an error: most
/// projects have no `skills/` at all.
fn collect_from_root(dir: &Path, source: &SkillSource, out: &mut Vec<(PathBuf, SkillSource)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        debug!("skills: no readable directory at {}", dir.display());
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let nested = path.join("SKILL.md");
            if nested.is_file() {
                out.push((nested, source.clone()));
            }
            continue;
        }
        if path.extension().is_some_and(|ext| ext == "md") {
            out.push((path, source.clone()));
        }
    }
}
