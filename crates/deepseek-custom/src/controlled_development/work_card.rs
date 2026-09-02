use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{WorkCardValidationError, WorkCardValidationErrors};

pub const MAX_WORK_CARD_ID_CHARS: usize = 128;
pub const MAX_WORK_CARD_OUTCOME_CHARS: usize = 500;
pub const MAX_PROOF_COMMANDS: usize = 3;
pub const MAX_PROOF_COMMAND_CHARS: usize = 2_000;
pub const MAX_PRODUCTION_PATHS: usize = 3;
pub const MAX_SUPPORTING_PATHS: usize = 64;
pub const MAX_WORK_CARD_PATH_CHARS: usize = 512;
pub const MAX_EXCLUSIONS: usize = 32;
pub const MAX_COMPLEXITY_EXCEPTIONS: usize = 32;
pub const MAX_WORK_CARD_ITEM_CHARS: usize = 500;

/// The complete, closed authority proposed by one planning run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkCard {
    pub id: String,
    pub outcome: String,
    pub proof_commands: Vec<String>,
    pub production_paths: Vec<String>,
    pub supporting_paths: Vec<String>,
    pub excluded: Vec<String>,
    pub complexity_exceptions: Vec<String>,
}

impl WorkCard {
    pub fn validate(&self) -> Result<(), WorkCardValidationErrors> {
        let mut errors = Vec::new();
        validate_single_line(
            "id",
            &self.id,
            MAX_WORK_CARD_ID_CHARS,
            "id must be nonempty and contain no line breaks",
            &mut errors,
        );
        validate_single_line(
            "outcome",
            &self.outcome,
            MAX_WORK_CARD_OUTCOME_CHARS,
            "outcome must name one nonempty observable result on one line",
            &mut errors,
        );
        if !self.outcome.trim().chars().any(char::is_alphanumeric) {
            errors.push(WorkCardValidationError::new(
                "outcome",
                "outcome must contain an observable alphanumeric result",
            ));
        }
        validate_commands(&self.proof_commands, &mut errors);
        validate_paths(
            "production_paths",
            &self.production_paths,
            MAX_PRODUCTION_PATHS,
            false,
            &mut errors,
        );
        validate_paths(
            "supporting_paths",
            &self.supporting_paths,
            MAX_SUPPORTING_PATHS,
            true,
            &mut errors,
        );
        validate_distinct_items("excluded", &self.excluded, 1, MAX_EXCLUSIONS, &mut errors);
        validate_distinct_items(
            "complexity_exceptions",
            &self.complexity_exceptions,
            0,
            MAX_COMPLEXITY_EXCEPTIONS,
            &mut errors,
        );
        validate_path_list_overlap(self, &mut errors);
        WorkCardValidationErrors::from_errors(errors)
    }
}

fn validate_commands(commands: &[String], errors: &mut Vec<WorkCardValidationError>) {
    if !(1..=MAX_PROOF_COMMANDS).contains(&commands.len()) {
        errors.push(WorkCardValidationError::new(
            "proof_commands",
            "proof_commands must contain between one and three commands",
        ));
    }
    for (index, command) in commands.iter().enumerate() {
        if command.trim().is_empty() {
            errors.push(WorkCardValidationError::new(
                format!("proof_commands[{index}]"),
                "proof command must be nonempty",
            ));
        }
        if command.chars().count() > MAX_PROOF_COMMAND_CHARS {
            errors.push(WorkCardValidationError::new(
                format!("proof_commands[{index}]"),
                format!("proof command must contain at most {MAX_PROOF_COMMAND_CHARS} characters"),
            ));
        }
        if command.chars().any(|character| character == '\0') {
            errors.push(WorkCardValidationError::new(
                format!("proof_commands[{index}]"),
                "proof command must not contain a null character",
            ));
        }
    }
}

fn validate_paths(
    field: &str,
    paths: &[String],
    maximum: usize,
    supporting_only: bool,
    errors: &mut Vec<WorkCardValidationError>,
) {
    if paths.len() > maximum {
        errors.push(WorkCardValidationError::new(
            field,
            format!("{field} must contain at most {maximum} paths"),
        ));
    }
    let mut seen = HashSet::new();
    for (index, path) in paths.iter().enumerate() {
        let item_field = format!("{field}[{index}]");
        if let Some(message) = invalid_repository_path(path) {
            errors.push(WorkCardValidationError::new(item_field.clone(), message));
        } else if supporting_only && !is_supporting_path(path) {
            errors.push(WorkCardValidationError::new(
                item_field.clone(),
                "supporting path must identify a test, fixture, document, or generated-state file",
            ));
        }
        if !seen.insert(path.to_lowercase()) {
            errors.push(WorkCardValidationError::new(
                item_field,
                "path duplicates another entry in the same list",
            ));
        }
    }
}

fn invalid_repository_path(path: &str) -> Option<String> {
    if path.is_empty() {
        return Some("path must be nonempty".into());
    }
    if path.chars().count() > MAX_WORK_CARD_PATH_CHARS {
        return Some(format!(
            "path must contain at most {MAX_WORK_CARD_PATH_CHARS} characters"
        ));
    }
    if path.contains('\\') {
        return Some("path must use normalized forward-slash separators".into());
    }
    if Path::new(path).is_absolute()
        || path.starts_with('/')
        || path.as_bytes().get(1) == Some(&b':')
    {
        return Some("path must be repository-relative".into());
    }
    if path
        .chars()
        .any(|character| matches!(character, '*' | '?' | '[' | ']' | '{' | '}'))
    {
        return Some("path must not contain glob syntax".into());
    }
    if path
        .chars()
        .any(|character| character.is_control() || character == ':')
    {
        return Some("path contains a character that is not valid in a normalized path".into());
    }
    if path
        .split('/')
        .any(|component| component.is_empty() || matches!(component, "." | ".."))
    {
        return Some("path must not contain empty, current, or parent components".into());
    }
    None
}

fn is_supporting_path(path: &str) -> bool {
    let normalized = path.to_ascii_lowercase();
    let components = normalized.split('/').collect::<Vec<_>>();
    let file_name = components.last().copied().unwrap_or_default();
    components.iter().any(|component| {
        matches!(
            *component,
            "test" | "tests" | "fixtures" | "fixture" | "docs" | "generated" | "assets"
        )
    }) || file_name.contains(".test.")
        || file_name.contains(".spec.")
        || file_name.ends_with("_test.rs")
        || file_name.ends_with("_tests.rs")
        || matches!(
            file_name,
            "readme.md" | "agents.md" | "claude.md" | "project_state.md"
        )
        || [".md", ".rst", ".adoc"]
            .iter()
            .any(|extension| file_name.ends_with(extension))
}

fn validate_distinct_items(
    field: &str,
    items: &[String],
    minimum: usize,
    maximum: usize,
    errors: &mut Vec<WorkCardValidationError>,
) {
    if items.len() < minimum {
        errors.push(WorkCardValidationError::new(
            field,
            format!("{field} must contain at least {minimum} entry"),
        ));
    }
    if items.len() > maximum {
        errors.push(WorkCardValidationError::new(
            field,
            format!("{field} must contain at most {maximum} entries"),
        ));
    }
    let mut seen = HashSet::new();
    for (index, item) in items.iter().enumerate() {
        let item_field = format!("{field}[{index}]");
        validate_single_line(
            &item_field,
            item,
            MAX_WORK_CARD_ITEM_CHARS,
            &format!("{field} entry must be nonempty and contain no line breaks"),
            errors,
        );
        if !seen.insert(item.to_lowercase()) {
            errors.push(WorkCardValidationError::new(
                item_field,
                format!("{field} entry duplicates another entry"),
            ));
        }
    }
}

fn validate_single_line(
    field: &str,
    value: &str,
    maximum: usize,
    empty_or_multiline_message: &str,
    errors: &mut Vec<WorkCardValidationError>,
) {
    if value.trim().is_empty() || value.lines().count() != 1 || value.contains(['\r', '\n']) {
        errors.push(WorkCardValidationError::new(
            field,
            empty_or_multiline_message,
        ));
    }
    if value.chars().count() > maximum {
        errors.push(WorkCardValidationError::new(
            field,
            format!("{field} must contain at most {maximum} characters"),
        ));
    }
}

fn validate_path_list_overlap(card: &WorkCard, errors: &mut Vec<WorkCardValidationError>) {
    let production = card
        .production_paths
        .iter()
        .map(|path| path.to_lowercase())
        .collect::<HashSet<_>>();
    for (index, path) in card.supporting_paths.iter().enumerate() {
        if production.contains(&path.to_lowercase()) {
            errors.push(WorkCardValidationError::new(
                format!("supporting_paths[{index}]"),
                "path overlaps production_paths",
            ));
        }
    }
}
