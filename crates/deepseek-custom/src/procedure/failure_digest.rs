//! Privacy-limited deterministic failure evidence for repair prompts.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{RepairTier, VerifierCommandDisposition, VerifierCommandResult};

/// Maximum detailed diagnostic retained by one failure digest.
pub const FAILURE_DIAGNOSTIC_CHARACTER_CAP: usize = 4_096;

/// Maximum command text retained by one failure digest.
pub const FAILURE_COMMAND_CHARACTER_CAP: usize = 512;

/// Default total size of the rendered failure section in a repair prompt.
pub const DEFAULT_FAILURE_SECTION_CHARACTER_CAP: usize = 8_192;

const MIN_FAILURE_SECTION_CHARACTER_CAP: usize = 128;

const TRUNCATION_MARKER: &str = "...[truncated]";

/// Stable typed category for a failed deterministic gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureDigestErrorCategory {
    VerifierFailed,
    VerifierSpawnFailed,
    Interrupted,
}

impl FailureDigestErrorCategory {
    pub const fn name(self) -> &'static str {
        match self {
            Self::VerifierFailed => "verifier_failed",
            Self::VerifierSpawnFailed => "verifier_spawn_failed",
            Self::Interrupted => "interrupted",
        }
    }
}

/// The only verifier evidence allowed to cross into a later model request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureDigest {
    pub attempt_number: u8,
    pub tier: RepairTier,
    pub command: String,
    pub exit_code: Option<i32>,
    pub error_category: FailureDigestErrorCategory,
    pub diagnostic: String,
}

impl FailureDigest {
    /// Build a digest from the bounded output retained by the verifier runner.
    pub fn from_verifier_result(
        attempt_number: u8,
        tier: RepairTier,
        result: &VerifierCommandResult,
    ) -> Self {
        let error_category = match result.disposition {
            VerifierCommandDisposition::Passed | VerifierCommandDisposition::Failed => {
                FailureDigestErrorCategory::VerifierFailed
            }
            VerifierCommandDisposition::SpawnFailed => {
                FailureDigestErrorCategory::VerifierSpawnFailed
            }
            VerifierCommandDisposition::Interrupted => FailureDigestErrorCategory::Interrupted,
        };
        let raw_diagnostic = if !result.combined_output.text.trim().is_empty() {
            result.combined_output.text.as_str()
        } else {
            result.error.as_deref().unwrap_or("no verifier diagnostic")
        };

        Self {
            attempt_number,
            tier,
            command: truncate_chars(
                &sanitize_command(&result.command),
                FAILURE_COMMAND_CHARACTER_CAP,
            ),
            exit_code: result.exit_code,
            error_category,
            diagnostic: truncate_chars(
                &sanitize_diagnostic(raw_diagnostic),
                FAILURE_DIAGNOSTIC_CHARACTER_CAP,
            ),
        }
    }

    fn summary_line(&self) -> String {
        format!(
            "attempt={} tier={} category={} exit_code={} command={}",
            self.attempt_number,
            self.tier,
            self.error_category.name(),
            display_exit_code(self.exit_code),
            one_line(&self.command),
        )
    }

    fn detailed_block(&self) -> String {
        format!("{}\ndiagnostic:\n{}", self.summary_line(), self.diagnostic)
    }
}

/// Invalid failure-section bounds.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FailureDigestSectionError {
    #[error("failure-section character cap must be at least {minimum}, got {actual}")]
    CharacterCapTooSmall { minimum: usize, actual: usize },
}

/// Render failures in deterministic tier and attempt order.
///
/// Older failures are one line each. The newest failure keeps detailed output.
/// When the total cap is tight, older summaries are omitted before the newest
/// failure is truncated.
pub fn build_failure_digest_section(
    digests: &[FailureDigest],
    character_cap: usize,
) -> Result<String, FailureDigestSectionError> {
    if character_cap < MIN_FAILURE_SECTION_CHARACTER_CAP {
        return Err(FailureDigestSectionError::CharacterCapTooSmall {
            minimum: MIN_FAILURE_SECTION_CHARACTER_CAP,
            actual: character_cap,
        });
    }
    if digests.is_empty() {
        return Ok(truncate_chars(
            "No prior deterministic failures.",
            character_cap,
        ));
    }

    let mut ordered = digests.iter().enumerate().collect::<Vec<_>>();
    ordered.sort_by_key(|(position, digest)| {
        (tier_order(digest.tier), digest.attempt_number, *position)
    });
    let (_, newest) = ordered.pop().expect("nonempty failures have a newest item");
    let newest_block = format!("Newest failure:\n{}", newest.detailed_block());

    let mut retained_older = Vec::new();
    let fixed_separator = "\n\n";
    for (_, digest) in ordered.into_iter().rev() {
        let line = digest.summary_line();
        let candidate_prefix = if retained_older.is_empty() {
            format!("Older failures:\n{line}")
        } else {
            format!("Older failures:\n{}\n{line}", retained_older.join("\n"))
        };
        if candidate_prefix.chars().count()
            + fixed_separator.chars().count()
            + newest_block.chars().count()
            <= character_cap
        {
            retained_older.push(line);
        } else {
            break;
        }
    }
    retained_older.reverse();

    let prefix = if retained_older.is_empty() {
        String::new()
    } else {
        format!("Older failures:\n{}\n\n", retained_older.join("\n"))
    };
    let remaining = character_cap.saturating_sub(prefix.chars().count());
    Ok(format!(
        "{}{}",
        prefix,
        truncate_chars(&newest_block, remaining)
    ))
}

fn tier_order(tier: RepairTier) -> u8 {
    match tier {
        RepairTier::Local => 0,
        RepairTier::Frontier => 1,
    }
}

fn display_exit_code(exit_code: Option<i32>) -> String {
    exit_code.map_or_else(|| "none".to_string(), |code| code.to_string())
}

fn one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn sanitize_command(command: &str) -> String {
    let mut redact_next = false;
    command
        .split_whitespace()
        .map(|token| {
            if redact_next {
                redact_next = false;
                return "[redacted]".to_string();
            }
            let lower = token.to_ascii_lowercase();
            if let Some((name, _)) = token.split_once('=')
                && is_sensitive_name(name.trim_start_matches('-').to_ascii_lowercase().as_str())
            {
                return format!("{name}=[redacted]");
            }
            if is_sensitive_name(lower.trim_start_matches('-')) {
                redact_next = true;
                return token.to_string();
            }
            token.to_string()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn sanitize_diagnostic(diagnostic: &str) -> String {
    let mut redacted = Vec::new();
    let mut redact_bearer = false;
    for line in diagnostic.replace("\r\n", "\n").lines() {
        let trimmed = line.trim_start();
        let lower = trimmed.to_ascii_lowercase();
        if is_forbidden_context_line(&lower) || is_source_or_diff_line(trimmed) {
            continue;
        }
        let mut words = Vec::new();
        for word in line.split_whitespace() {
            if redact_bearer {
                words.push("[redacted]".to_string());
                redact_bearer = false;
                continue;
            }
            if word.eq_ignore_ascii_case("bearer") {
                words.push(word.to_string());
                redact_bearer = true;
                continue;
            }
            if let Some((name, _)) = word.split_once('=')
                && is_sensitive_name(name.to_ascii_lowercase().as_str())
            {
                words.push(format!("{name}=[redacted]"));
                continue;
            }
            words.push(word.to_string());
        }
        if !words.is_empty() {
            redacted.push(words.join(" "));
        }
    }
    if redacted.is_empty() {
        "diagnostic omitted by privacy filter".to_string()
    } else {
        redacted.join("\n")
    }
}

fn is_sensitive_name(name: &str) -> bool {
    [
        "api_key",
        "apikey",
        "token",
        "password",
        "secret",
        "credential",
    ]
    .iter()
    .any(|sensitive| name == *sensitive || name.ends_with(&format!("_{sensitive}")))
}

fn is_forbidden_context_line(lower: &str) -> bool {
    [
        "source_content",
        "source contents",
        "prior_prompt",
        "prior prompt",
        "chat_history",
        "chat history",
        "conversation_history",
        "conversation history",
        "assistant_output",
        "model_output",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn is_source_or_diff_line(line: &str) -> bool {
    line.starts_with("diff --git ")
        || line.starts_with("--- ")
        || line.starts_with("+++ ")
        || line.starts_with("@@ ")
        || line.starts_with("+ ")
        || line.starts_with("- ")
}

fn truncate_chars(value: &str, cap: usize) -> String {
    let length = value.chars().count();
    if length <= cap {
        return value.to_string();
    }
    if cap <= TRUNCATION_MARKER.chars().count() {
        return TRUNCATION_MARKER.chars().take(cap).collect();
    }
    let keep = cap - TRUNCATION_MARKER.chars().count();
    format!(
        "{}{}",
        value.chars().take(keep).collect::<String>(),
        TRUNCATION_MARKER
    )
}
