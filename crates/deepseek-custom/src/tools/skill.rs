//! The `Skill` tool: read one skill's instructions on demand.
//!
//! The system prompt carries an index of every skill's name and a trimmed
//! description, never a body. See `src/skills/mod.rs` for the measurement
//! that forces that split. This tool is the other half: the model reads the
//! index, picks a skill, and calls this to get the instructions it must
//! actually follow.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use tracing::info;

use crate::error::Result;
use crate::skills::Skill;
use crate::tools::{Tool, ToolOutput};

/// Reads a named skill's body off disk.
pub struct SkillTool {
    skills: Arc<Vec<Skill>>,
}

impl SkillTool {
    pub fn new(skills: Arc<Vec<Skill>>) -> Self {
        Self { skills }
    }

    /// Find a skill by name, case-insensitively. Skill names are written by
    /// hand in frontmatter and quoted back by a model, so an exact-case
    /// match alone would fail on `Tighten` against `tighten`.
    fn find(&self, name: &str) -> Option<&Skill> {
        self.skills
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(name))
    }

    /// The error a bad name comes back as, listing what does exist. A model
    /// that guessed cannot correct itself without seeing the real names.
    fn unknown_name_error(&self, name: &str) -> String {
        let mut known: Vec<&str> = self.skills.iter().map(|s| s.name.as_str()).collect();
        known.sort_unstable();
        format!(
            "no skill named \"{name}\" (known: {})",
            if known.is_empty() {
                "none loaded".to_string()
            } else {
                known.join(", ")
            }
        )
    }
}

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        "Skill"
    }

    fn description(&self) -> &str {
        "Read the full instructions of a skill listed in the Available \
         Skills index. Call this before following a skill, since the index \
         carries only names and one-line summaries. Returns the skill's \
         markdown body."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The skill's name, exactly as listed in the Available Skills index."
                }
            },
            "required": ["name"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput> {
        let Some(name) = input.get("name").and_then(|v| v.as_str()) else {
            return Ok(ToolOutput::error("missing required parameter \"name\""));
        };

        let Some(skill) = self.find(name) else {
            return Ok(ToolOutput::error(self.unknown_name_error(name)));
        };

        // A read failure here is a tool error, not a hard failure: the file
        // was there at discovery time and has since moved or become
        // unreadable, which the model can report but the turn survives.
        let body = match skill.load_body() {
            Ok(body) => body,
            Err(e) => {
                return Ok(ToolOutput::error(format!(
                    "failed to read skill \"{}\" from {}: {e}",
                    skill.name,
                    skill.path.display()
                )));
            }
        };

        info!(
            "skill loaded: {} ({}, {} chars)",
            skill.name,
            skill.source.label(),
            body.len()
        );
        Ok(ToolOutput {
            content: format!("# Skill: {}\n\n{body}", skill.name),
            is_error: false,
            image: None,
        })
    }
}
