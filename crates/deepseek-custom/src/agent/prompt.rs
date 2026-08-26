use crate::api::types::ToolDef;

/// Configurable system-prompt builder retained as public API.
pub struct SystemPromptBuilder {
    base_instructions: String,
    current_date: String,
}

impl Default for SystemPromptBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemPromptBuilder {
    pub fn new() -> Self {
        Self {
            base_instructions: default_base_instructions(),
            current_date: chrono_now_or_empty(),
        }
    }

    /// Set custom base instructions instead of the defaults.
    pub fn with_base_instructions(mut self, instructions: String) -> Self {
        self.base_instructions = instructions;
        self
    }

    /// Set the date text included in the prompt.
    pub fn with_date(mut self, date: String) -> Self {
        self.current_date = date;
        self
    }

    pub fn build(
        &self,
        memory_fragment: Option<&str>,
        skills_fragment: Option<&str>,
        tools: &[ToolDef],
    ) -> String {
        assemble_system_prompt(
            &self.base_instructions,
            &self.current_date,
            memory_fragment,
            skills_fragment,
            tools,
        )
    }
}

/// Assemble a system prompt using the default instructions and current date.
pub fn build_system_prompt(
    memory_fragment: Option<&str>,
    skills_fragment: Option<&str>,
    tools: &[ToolDef],
) -> String {
    SystemPromptBuilder::new().build(memory_fragment, skills_fragment, tools)
}

/// This does not report a working directory. That line is added by
/// `MessageHistory`, re-read every turn from the shared `working_dir` the
/// tools also read from, so the model is never told a directory it built
/// once at startup and never revisited. See `MessageHistory::set_working_dir`.
fn assemble_system_prompt(
    base_instructions: &str,
    current_date: &str,
    memory_fragment: Option<&str>,
    skills_fragment: Option<&str>,
    tools: &[ToolDef],
) -> String {
    let mut parts: Vec<String> = Vec::new();

    // 1. Date. The working directory is not built in here: it is added
    // by `MessageHistory` on every turn, see the function doc comment.
    parts.push(format!("Today's date is {current_date}.\n"));

    // 2. Base instructions
    parts.push(base_instructions.to_string());

    // 3. Tool-first arithmetic (unconditional, see Phase A of the
    // diversity implementation plan)
    parts.push(tool_first_arithmetic_instructions().to_string());

    // 4. Memory files (CLAUDE.md, MEMORY.md)
    if let Some(mem) = memory_fragment
        && !mem.is_empty()
    {
        parts.push(format!("\n## Project Context\n\n{mem}"));
    }

    // 5. Skills
    if let Some(skills) = skills_fragment
        && !skills.is_empty()
    {
        parts.push(format!("\n## Available Skills\n\n{skills}"));
        parts.push(slash_command_instructions().to_string());
    }

    // 6. Tool definitions
    if !tools.is_empty() {
        let tool_json = serde_json::to_string_pretty(tools).unwrap_or_default();
        parts.push(format!(
            "\n## Available Tools\n\nYou have access to the following tools. Use them by responding with a tool_call:\n\n```json\n{tool_json}\n```"
        ));
    }

    parts.join("\n")
}

fn default_base_instructions() -> String {
    r"You are DeepSeekCustom, an AI coding assistant built on the DeepSeek harness.

You have access to tools for reading files, writing files, executing shell commands, and managing your session.

General guidelines:
- Be concise and accurate in your responses
- When writing code, follow the existing project conventions
- Use tools proactively when they would help answer the user's question
- Read files before modifying them
- Report errors clearly when they occur"
        .to_string()
}

/// Instruction block appended to the system prompt when replies are spoken
/// aloud through text to speech. Tells the model to answer like a person
/// talking, not like a document.
pub fn voice_mode_instructions() -> &'static str {
    r#"## Voice reply mode

Every reply is spoken aloud, so it must sound like speech, not like a document.
Answer in at most two sentences. This is a hard cap, not a target.
If the full answer does not fit, give the short answer and offer to go into detail if asked.
No markdown, no headings, no bullet lists, no numbered lists, no tables.
No code blocks and no code. Describe what the code does instead.
Do not read file paths, URLs, or long identifiers aloud. Name the file plainly, for example "the agent loop file".
Use short everyday words and a conversational cadence, the way a person answers a question out loud.
Tool use is unchanged. Only the text spoken back to the user is constrained."#
}

/// Instruction block appended right after the skill index. Says what a
/// leading slash means, because nothing else does.
///
/// This harness has no slash commands of its own. A task written for Claude
/// Code says things like "/commit your work", and a real autopilot run read
/// that line, had no way to act on it, and finished every item without ever
/// committing. The name after the slash is a skill name, and the `skill`
/// tool is how a skill body gets read, so saying that once turns a dead
/// instruction into a live one.
pub fn slash_command_instructions() -> &'static str {
    r"## Slash commands are skills

A word written with a leading slash, such as `/commit` or `/review`, names a
skill in the index above. It is not a command this harness runs for you.
Call the `skill` tool with that name to read its instructions, then follow
them yourself with your own tools. A slash name missing from the index above
is not available: say so plainly rather than pretending the step happened."
}

/// Instruction block appended to the system prompt unconditionally. Tells
/// the model to run arithmetic through a tool rather than from memory, since
/// an exact count or sum that is wrong costs real time and money.
pub fn tool_first_arithmetic_instructions() -> &'static str {
    r"## Tool-first arithmetic

When you need an exact calculation -- counts, date math, sums that matter --
run it through `Bash` with a one-line command (Python or shell), not from
memory. Skip this only for the simplest mental math."
}

fn chrono_now_or_empty() -> String {
    // Simple date without chrono dependency, just use UTC timestamp
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
