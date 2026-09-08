//! Turning discovered skill files into `Skill` values.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tracing::{debug, warn};

use super::Skill;
use super::discovery::{SkillSource, discover_skill_files};

/// Loads skills from the filesystem.
pub struct SkillLoader;

impl SkillLoader {
    /// Every skill reachable from this project: enabled plugins, the
    /// user's `~/.claude/skills/`, and `<project_root>/skills/`. A name
    /// claimed twice resolves in that order, so a project skill wins over
    /// a global one and a global one wins over a plugin's.
    ///
    /// Only the frontmatter of each file is parsed. Bodies stay on disk
    /// until the `Skill` tool asks for one.
    pub fn load(project_root: &Path) -> Vec<Skill> {
        let files = discover_skill_files(project_root);
        Self::parse_all(files)
    }

    /// Parse an already-discovered file list. Split out so a test can hand
    /// in a fixture list instead of whatever the machine has installed.
    pub fn parse_all(files: Vec<(PathBuf, SkillSource)>) -> Vec<Skill> {
        let mut by_name: HashMap<String, Skill> = HashMap::new();
        let mut order: Vec<String> = Vec::new();

        for (path, source) in files {
            let Some(skill) = parse_one(&path, source) else {
                continue;
            };
            if !by_name.contains_key(&skill.name) {
                order.push(skill.name.clone());
            }
            by_name.insert(skill.name.clone(), skill);
        }

        order
            .into_iter()
            .filter_map(|name| by_name.remove(&name))
            .collect()
    }
}

/// Read one skill file and parse its frontmatter. A file that cannot be
/// read or parsed is skipped with a `warn` rather than failing the whole
/// load, so one bad skill never hides the other 98.
fn parse_one(path: &Path, source: SkillSource) -> Option<Skill> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            warn!("skills: failed to read {}: {e}", path.display());
            return None;
        }
    };
    match Skill::from_markdown(&content, path, source) {
        Ok(skill) => {
            debug!("skills: loaded {} from {}", skill.name, path.display());
            Some(skill)
        }
        Err(e) => {
            warn!("skills: failed to parse {}: {e}", path.display());
            None
        }
    }
}
