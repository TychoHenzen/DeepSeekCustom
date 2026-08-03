use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;
use tracing::{debug, warn};

use crate::error::Result;

/// A parsed skill from a markdown file.
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub tools: Vec<String>,
    pub content: String,
}

/// YAML frontmatter found in skill markdown files.
#[derive(Deserialize, Default)]
struct SkillFrontmatter {
    name: Option<String>,
    description: Option<String>,
    tools: Option<Vec<String>>,
}

impl Skill {
    /// Parse a skill from markdown content.
    ///
    /// Expects optional YAML frontmatter between `---` delimiters.
    /// Falls back to filename if no `name:` in frontmatter.
    pub fn from_markdown(content: &str, filename: &str) -> Result<Skill> {
        let (frontmatter_str, body) = split_frontmatter(content);

        let fm: SkillFrontmatter = if let Some(fm_str) = frontmatter_str {
            serde_yaml::from_str(fm_str).unwrap_or_default()
        } else {
            SkillFrontmatter::default()
        };

        let name = fm.name.unwrap_or_else(|| {
            // Strip .md extension from filename
            filename.strip_suffix(".md").unwrap_or(filename).to_string()
        });

        let description = fm.description.unwrap_or_default();
        let tools = fm.tools.unwrap_or_default();

        Ok(Skill {
            name,
            description,
            tools,
            content: body.to_string(),
        })
    }
}

/// Split content into (frontmatter, body) if frontmatter exists.
fn split_frontmatter(content: &str) -> (Option<&str>, &str) {
    let trimmed = content.trim();
    if !trimmed.starts_with("---") {
        return (None, trimmed);
    }

    let after_first = &trimmed[3..];
    if let Some(end) = after_first.find("\n---") {
        let fm = after_first[..end].trim();
        let body = after_first[end + 4..].trim();
        (Some(fm), body)
    } else {
        (None, trimmed)
    }
}

/// Loads skills from the filesystem.
pub struct SkillLoader;

impl SkillLoader {
    /// Load all skills from `<project_root>/skills/*.md`.
    pub fn load_all(project_root: &Path) -> Result<Vec<Skill>> {
        let dir = project_root.join("skills");
        if !dir.exists() {
            debug!("skills directory not found: {}", dir.display());
            return Ok(Vec::new());
        }
        Self::load_from_dir(&dir)
    }

    /// Load all skills from `~/.claude/skills/*.md`.
    pub fn load_global() -> Result<Vec<Skill>> {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_default();
        let dir = Path::new(&home).join(".claude").join("skills");
        if !dir.exists() {
            debug!("global skills directory not found: {}", dir.display());
            return Ok(Vec::new());
        }
        Self::load_from_dir(&dir)
    }

    /// Merge project and global skills. Project overrides global by name.
    pub fn merge(project: Vec<Skill>, global: Vec<Skill>) -> Vec<Skill> {
        let mut map: HashMap<String, Skill> = HashMap::new();

        for skill in global {
            map.insert(skill.name.clone(), skill);
        }
        for skill in project {
            map.insert(skill.name.clone(), skill);
        }

        map.into_values().collect()
    }

    fn load_from_dir(dir: &Path) -> Result<Vec<Skill>> {
        let mut skills = Vec::new();

        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) => {
                warn!("failed to read skills dir {}: {e}", dir.display());
                return Ok(Vec::new());
            }
        };

        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    warn!("skills: failed to read entry: {e}");
                    continue;
                }
            };

            let path = entry.path();
            if path.extension().map_or(true, |ext| ext != "md") {
                continue;
            }

            let filename = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();

            match std::fs::read_to_string(&path) {
                Ok(content) => match Skill::from_markdown(&content, &filename) {
                    Ok(skill) => {
                        debug!("loaded skill: {}", skill.name);
                        skills.push(skill);
                    }
                    Err(e) => {
                        warn!("failed to parse skill {}: {e}", path.display());
                    }
                },
                Err(e) => {
                    warn!("failed to read skill {}: {e}", path.display());
                }
            }
        }

        Ok(skills)
    }
}

/// Format skills into a string for the system prompt.
pub fn format_skills_for_prompt(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return String::new();
    }

    let mut parts = Vec::new();

    for skill in skills {
        let mut section = format!("### {}\n", skill.name);
        if !skill.description.is_empty() {
            section.push_str(&format!("_{}_\n\n", skill.description));
        }
        if !skill.tools.is_empty() {
            section.push_str(&format!("**Tools:** {}\n\n", skill.tools.join(", ")));
        }
        section.push_str(&skill.content);
        parts.push(section);
    }

    parts.join("\n\n---\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_SKILL: &str = r#"---
name: my-skill
description: A test skill
tools:
  - bash
  - read
---

# Instructions

Do the thing carefully."#;

    const NO_FRONTMATTER: &str = r#"# Plain skill

Just raw markdown content."#;

    #[test]
    fn parse_skill_with_frontmatter() {
        let skill = Skill::from_markdown(SAMPLE_SKILL, "test.md").unwrap();
        assert_eq!(skill.name, "my-skill");
        assert_eq!(skill.description, "A test skill");
        assert_eq!(skill.tools, vec!["bash", "read"]);
        assert!(skill.content.contains("Do the thing carefully"));
    }

    #[test]
    fn parse_skill_without_frontmatter() {
        let skill = Skill::from_markdown(NO_FRONTMATTER, "plain.md").unwrap();
        assert_eq!(skill.name, "plain"); // filename without .md
        assert!(skill.content.contains("Just raw markdown"));
    }

    #[test]
    fn format_skills_produces_output() {
        let skills = vec![Skill {
            name: "test".into(),
            description: "desc".into(),
            tools: vec!["bash".into()],
            content: "Do stuff".into(),
        }];
        let formatted = format_skills_for_prompt(&skills);
        assert!(formatted.contains("### test"));
        assert!(formatted.contains("Do stuff"));
    }

    #[test]
    fn format_empty_skills_returns_empty() {
        assert_eq!(format_skills_for_prompt(&[]), "");
    }

    #[test]
    fn malformed_skill_handled_gracefully() {
        // Missing closing --- delimiter
        let content = "---\nname: broken\n";
        let skill = Skill::from_markdown(content, "broken.md").unwrap();
        // Falls through — frontmatter delimiter not found
        assert!(skill.name == "broken" || !skill.content.is_empty());
    }
}
