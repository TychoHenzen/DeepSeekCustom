use crate::api::types::ToolDef;

/// Builds the system prompt from base instructions, memory files, skills, and tool definitions.
pub struct SystemPromptBuilder {
    base_instructions: String,
    current_date: String,
    working_dir: String,
}

impl SystemPromptBuilder {
    pub fn new() -> Self {
        Self {
            base_instructions: default_base_instructions(),
            current_date: chrono_now_or_empty(),
            working_dir: std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "unknown".into()),
        }
    }

    /// Set custom base instructions (overrides defaults).
    pub fn with_base_instructions(mut self, instructions: String) -> Self {
        self.base_instructions = instructions;
        self
    }

    /// Set current date.
    pub fn with_date(mut self, date: String) -> Self {
        self.current_date = date;
        self
    }

    /// Assemble the full system prompt.
    pub fn build(
        &self,
        memory_fragment: Option<&str>,
        skills_fragment: Option<&str>,
        tools: &[ToolDef],
    ) -> String {
        let mut parts: Vec<String> = Vec::new();

        // 1. Date and working directory
        parts.push(format!(
            "Today's date is {}.\nWorking directory: {}.\n",
            self.current_date, self.working_dir
        ));

        // 2. Base instructions
        parts.push(self.base_instructions.clone());

        // 3. Memory files (CLAUDE.md, MEMORY.md)
        if let Some(mem) = memory_fragment {
            if !mem.is_empty() {
                parts.push(format!("\n## Project Context\n\n{mem}"));
            }
        }

        // 4. Skills
        if let Some(skills) = skills_fragment {
            if !skills.is_empty() {
                parts.push(format!("\n## Available Skills\n\n{skills}"));
            }
        }

        // 5. Tool definitions
        if !tools.is_empty() {
            let tool_json = serde_json::to_string_pretty(tools).unwrap_or_default();
            parts.push(format!(
                "\n## Available Tools\n\nYou have access to the following tools. Use them by responding with a tool_call:\n\n```json\n{tool_json}\n```"
            ));
        }

        parts.join("\n")
    }
}

fn default_base_instructions() -> String {
    r#"You are DeepSeekCustom, an AI coding assistant built on the DeepSeek harness.

You have access to tools for reading files, writing files, executing shell commands, and managing your session.

General guidelines:
- Be concise and accurate in your responses
- When writing code, follow the existing project conventions
- Use tools proactively when they would help answer the user's question
- Read files before modifying them
- Report errors clearly when they occur"#
        .to_string()
}

fn chrono_now_or_empty() -> String {
    // Simple date without chrono dependency — just use UTC timestamp
    // Format: YYYY-MM-DD
    use std::time::SystemTime;
    match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
        Ok(dur) => {
            let secs = dur.as_secs();
            let days = secs / 86400;
            // Crude but works for system prompt date (doesn't need to be exact)
            let year = 1970 + (days / 365) as i32;
            let remaining = days % 365;
            // Simple month approximation
            let month_days = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
            let mut month = 1;
            let mut day_remaining = remaining;
            for md in month_days {
                if day_remaining < md {
                    break;
                }
                day_remaining -= md;
                month += 1;
            }
            let day = day_remaining + 1;
            format!("{year:04}-{month:02}-{day:02}")
        }
        Err(_) => "unknown".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_includes_all_sections() {
        let builder = SystemPromptBuilder::new();
        let tools = vec![ToolDef {
            tool_type: "function".into(),
            function: crate::api::types::FunctionDef {
                name: "read".into(),
                description: "Read a file".into(),
                parameters: serde_json::json!({}),
            },
        }];
        let prompt = builder.build(Some("memory content"), Some("skill list"), &tools);

        assert!(prompt.contains("memory content"));
        assert!(prompt.contains("skill list"));
        assert!(prompt.contains("\"name\": \"read\""));
        assert!(prompt.contains("Working directory"));
    }

    #[test]
    fn builder_handles_empty_optionals() {
        let builder = SystemPromptBuilder::new();
        let prompt = builder.build(None, None, &[]);

        assert!(prompt.contains("DeepSeekCustom"));
        assert!(!prompt.contains("## Project Context"));
        assert!(!prompt.contains("## Available Skills"));
        assert!(!prompt.contains("## Available Tools"));
    }
}
