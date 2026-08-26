//! The `grep` tool: regular expression search across files.
//!
//! The third of the three file tools Claude Code has and this harness did
//! not. A real autopilot run reached for `powershell Select-String` through
//! `bash` instead, once per lookup, each one paying process startup and
//! shell quoting to answer "where is this symbol".

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::Deserialize;
use tracing::{debug, info};

use crate::error::{HarnessError, Result};
use crate::tools::{Tool, ToolOutput};

/// The most result lines one call reports, when the call names no limit of
/// its own.
const DEFAULT_HEAD_LIMIT: usize = 100;

/// Directory names never descended into. Build output and version control
/// state hold millions of lines that no search of this repository wants,
/// and walking them costs more than every real hit put together.
const SKIPPED_DIRS: [&str; 5] = ["target", ".git", "node_modules", ".deepseek", "models"];

/// What a search reports back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    /// One line per matching file path.
    FilesWithMatches,
    /// One line per match, as `path:line:text`.
    Content,
    /// One line per file, as `path:count`.
    Count,
}

impl OutputMode {
    /// Parse the wire value. An unknown value is a caller error, reported
    /// as such rather than silently falling back to a default the caller
    /// did not ask for.
    fn parse(value: Option<&str>) -> std::result::Result<Self, String> {
        match value {
            None | Some("files_with_matches") => Ok(Self::FilesWithMatches),
            Some("content") => Ok(Self::Content),
            Some("count") => Ok(Self::Count),
            Some(other) => Err(format!(
                "Unknown output_mode {other:?}. Use content, files_with_matches, or count."
            )),
        }
    }
}

/// Regular expression search under the working directory. Reads the shared
/// working directory fresh on every call, like the other file tools.
pub struct GrepTool {
    working_dir: Arc<Mutex<PathBuf>>,
}

impl GrepTool {
    pub fn new(working_dir: Arc<Mutex<PathBuf>>) -> Self {
        Self { working_dir }
    }

    fn search_root(&self, path: Option<&str>) -> PathBuf {
        let working_dir = self
            .working_dir
            .lock()
            .expect("working_dir mutex poisoned")
            .clone();
        match path {
            Some(path) if Path::new(path).is_absolute() => PathBuf::from(path),
            Some(path) => working_dir.join(path),
            None => working_dir,
        }
    }
}

#[derive(Deserialize)]
struct GrepInput {
    pattern: String,
    path: Option<String>,
    glob: Option<String>,
    #[serde(default)]
    case_insensitive: bool,
    output_mode: Option<String>,
    head_limit: Option<usize>,
}

/// Whether a walked entry is a file this search should read: not inside a
/// skipped directory, and matching the caller's glob filter when one was
/// given.
fn is_searchable(entry: &walkdir::DirEntry, filter: Option<&glob::Pattern>) -> bool {
    if !entry.file_type().is_file() {
        return false;
    }
    let Some(filter) = filter else {
        return true;
    };
    let path = entry.path().to_string_lossy().replace('\\', "/");
    let name = entry.file_name().to_string_lossy().to_string();
    filter.matches(&path) || filter.matches(&name)
}

/// Every match in one file, formatted for `mode`. A file that is not valid
/// UTF-8 yields nothing: it is a binary, and this tool searches text.
fn search_one_file(path: &Path, regex: &regex::Regex, mode: OutputMode) -> Vec<String> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let display = path.display();

    let hits: Vec<(usize, &str)> = content
        .lines()
        .enumerate()
        .filter(|(_, line)| regex.is_match(line))
        .collect();

    if hits.is_empty() {
        return Vec::new();
    }
    match mode {
        OutputMode::FilesWithMatches => vec![display.to_string()],
        OutputMode::Count => vec![format!("{display}:{}", hits.len())],
        OutputMode::Content => hits
            .iter()
            .map(|(index, line)| format!("{display}:{}:{}", index + 1, line.trim_end()))
            .collect(),
    }
}

/// Walk `root` and collect result lines, stopping once `limit` lines are
/// in hand. A directory the walker cannot read is skipped rather than
/// failing the search.
fn search_tree(
    root: &Path,
    regex: &regex::Regex,
    filter: Option<&glob::Pattern>,
    mode: OutputMode,
    limit: usize,
) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let walker = walkdir::WalkDir::new(root)
        .into_iter()
        .filter_entry(|entry| !is_skipped_dir(entry));

    for entry in walker.flatten() {
        if !is_searchable(&entry, filter) {
            continue;
        }
        lines.extend(search_one_file(entry.path(), regex, mode));
        if lines.len() >= limit {
            lines.truncate(limit);
            break;
        }
    }
    lines
}

/// Whether the walker should refuse to descend into this entry.
fn is_skipped_dir(entry: &walkdir::DirEntry) -> bool {
    if !entry.file_type().is_dir() {
        return false;
    }
    let name = entry.file_name().to_string_lossy();
    SKIPPED_DIRS.contains(&name.as_ref())
}

/// Build the regex, honouring the case-insensitive flag.
fn build_regex(pattern: &str, case_insensitive: bool) -> std::result::Result<regex::Regex, String> {
    regex::RegexBuilder::new(pattern)
        .case_insensitive(case_insensitive)
        .build()
        .map_err(|e| format!("Invalid regex: {e}"))
}

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Search file contents with a regular expression. Returns matching file paths by \
         default, or matching lines when output_mode is content. Searches the current working \
         directory unless a path is given, and never descends into target, .git, or \
         node_modules."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regular expression to search for"
                },
                "path": {
                    "type": "string",
                    "description": "File or directory to search in. Defaults to the current working directory."
                },
                "glob": {
                    "type": "string",
                    "description": "Only search files matching this glob, for example *.rs"
                },
                "case_insensitive": {
                    "type": "boolean",
                    "description": "Match without regard to case. Defaults to false."
                },
                "output_mode": {
                    "type": "string",
                    "enum": ["content", "files_with_matches", "count"],
                    "description": "content gives path:line:text, files_with_matches gives paths, count gives path:count. Defaults to files_with_matches."
                },
                "head_limit": {
                    "type": "integer",
                    "description": "Most result lines to return. Defaults to 100."
                }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput> {
        let parsed: GrepInput = serde_json::from_value(input)
            .map_err(|e| HarnessError::Tool(format!("Invalid grep input: {e}")))?;

        let root = self.search_root(parsed.path.as_deref());
        let mode = match OutputMode::parse(parsed.output_mode.as_deref()) {
            Ok(mode) => mode,
            Err(reason) => return Ok(ToolOutput::error(reason)),
        };
        let regex = match build_regex(&parsed.pattern, parsed.case_insensitive) {
            Ok(regex) => regex,
            Err(reason) => return Ok(ToolOutput::error(reason)),
        };
        let filter = match parsed.glob.as_deref().map(glob::Pattern::new).transpose() {
            Ok(filter) => filter,
            Err(e) => return Ok(ToolOutput::error(format!("Invalid glob filter: {e}"))),
        };

        let limit = parsed.head_limit.unwrap_or(DEFAULT_HEAD_LIMIT).max(1);
        debug!("grep: pattern={} root={}", parsed.pattern, root.display());
        let lines = search_tree(&root, &regex, filter.as_ref(), mode, limit);

        info!(
            "grep: {} result line(s) for {}",
            lines.len(),
            parsed.pattern
        );
        if lines.is_empty() {
            return Ok(ToolOutput {
                content: format!("No matches for {} under {}", parsed.pattern, root.display()),
                is_error: false,
                image: None,
            });
        }
        let capped = if lines.len() == limit {
            format!("\n(capped at {limit} results)")
        } else {
            String::new()
        };
        Ok(ToolOutput {
            content: format!("{}{capped}", lines.join("\n")),
            is_error: false,
            image: None,
        })
    }
}
