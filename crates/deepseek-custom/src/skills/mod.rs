//! Skill discovery and loading, in Claude Code's own on-disk format.
//!
//! A skill is a markdown file with YAML frontmatter. Two shapes exist and
//! both are read: a flat `<name>.md`, and a directory holding `SKILL.md`.
//! The directory shape is what Claude Code actually writes, and reading
//! only the flat one is why this harness used to see 4 skills on a machine
//! carrying 99.
//!
//! Bodies are not loaded here. `format_skills_for_prompt` emits a name and
//! a trimmed description per skill, and the `Skill` tool
//! (`src/tools/skill.rs`) reads one body off disk when the model asks for
//! it. That split is not a refinement, it is what makes the feature
//! possible at all: the 99 skills reachable on this machine hold 884 KB of
//! markdown, about 221000 tokens, against a default context budget of
//! 100000. Inlining every body would overrun the whole budget with the
//! system prompt before the first user message.

pub mod discovery;
pub mod loader;

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::Result;

pub use discovery::{SkillSource, discover_skill_files};
pub use loader::SkillLoader;

/// How much of a skill's description goes into the system prompt.
///
/// A description in this format runs to a paragraph or more, and the
/// prompt carries one per skill. The full text of 99 of them is its own
/// context problem. This keeps the index near 5000 tokens.
const DESCRIPTION_PROMPT_LIMIT: usize = 200;

/// A skill found on disk. Carries its metadata and where its body lives,
/// never the body itself. See the module docs for why.
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub tools: Vec<String>,
    /// The markdown file the body is read back from on demand.
    pub path: PathBuf,
    /// Which root this skill was found under, for the prompt index and for
    /// telling two same-named skills apart in a log line.
    pub source: SkillSource,
}

/// YAML frontmatter found in skill markdown files.
#[derive(Deserialize, Default)]
struct SkillFrontmatter {
    name: Option<String>,
    description: Option<String>,
    tools: Option<Vec<String>>,
    /// Claude Code's own spelling of the same field. Accepted so a skill
    /// written for Claude Code parses here unchanged.
    #[serde(rename = "allowed-tools")]
    allowed_tools: Option<Vec<String>>,
}

impl Skill {
    /// Parse a skill's metadata from markdown content.
    ///
    /// Expects optional YAML frontmatter between `---` delimiters.
    ///
    /// Falls back to the file's own name when the frontmatter carries no
    /// `name`. A `SKILL.md` falls back to its parent directory instead,
    /// since every such file is called the same thing.
    pub fn from_markdown(content: &str, path: &Path, source: SkillSource) -> Result<Skill> {
        let (frontmatter_str, _) = split_frontmatter(content);

        let fm: SkillFrontmatter = match frontmatter_str {
            Some(fm_str) => serde_yaml::from_str(fm_str).unwrap_or_default(),
            None => SkillFrontmatter::default(),
        };

        Ok(Skill {
            name: fm.name.unwrap_or_else(|| name_from_path(path)),
            description: fm.description.unwrap_or_default(),
            tools: fm.tools.or(fm.allowed_tools).unwrap_or_default(),
            path: path.to_path_buf(),
            source,
        })
    }

    /// Read this skill's body off disk, without its frontmatter.
    pub fn load_body(&self) -> Result<String> {
        let content = std::fs::read_to_string(&self.path)?;
        Ok(split_frontmatter(&content).1.to_string())
    }
}

/// The name a skill takes when its frontmatter declares none. A `SKILL.md`
/// is named for the directory holding it, because every one of them shares
/// that filename. Anything else is named for its own file, minus the
/// extension.
fn name_from_path(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    if !stem.eq_ignore_ascii_case("skill") {
        return stem;
    }
    path.parent()
        .and_then(Path::file_name)
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or(stem)
}

/// Split content into (frontmatter, body) if frontmatter exists.
pub fn split_frontmatter(content: &str) -> (Option<&str>, &str) {
    let trimmed = content.trim();
    if !trimmed.starts_with("---") {
        return (None, trimmed);
    }

    let after_first = &trimmed[3..];
    match after_first.find("\n---") {
        Some(end) => (
            Some(after_first[..end].trim()),
            after_first[end + 4..].trim(),
        ),
        None => (None, trimmed),
    }
}

/// Collapse a description onto one line and cap it, so a paragraph-long
/// description costs the prompt a line rather than a page.
fn trim_description(description: &str) -> String {
    let flat = description.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= DESCRIPTION_PROMPT_LIMIT {
        return flat;
    }
    let cut: String = flat.chars().take(DESCRIPTION_PROMPT_LIMIT).collect();
    format!("{}...", cut.trim_end())
}

/// Format the skill index for the system prompt: one line per skill,
/// naming it and saying what it is for. The body is deliberately absent.
/// The leading instruction is what makes the index usable, since a name
/// alone does not tell the model how to reach the rest.
pub fn format_skills_for_prompt(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return String::new();
    }

    let mut lines = Vec::with_capacity(skills.len() + 2);
    lines.push(
        "Call the `Skill` tool with a skill's name to read its full \
         instructions before following it. Only names and summaries are \
         listed here."
            .to_string(),
    );
    lines.push(String::new());

    let mut sorted: Vec<&Skill> = skills.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    for skill in sorted {
        let description = trim_description(&skill.description);
        if description.is_empty() {
            lines.push(format!("- `{}`", skill.name));
        } else {
            lines.push(format!("- `{}`: {}", skill.name, description));
        }
    }

    lines.join("\n")
}
