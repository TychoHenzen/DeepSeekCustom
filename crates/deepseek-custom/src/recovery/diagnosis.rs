//! Sanitized diagnostic prompts and strictly validated diagnostic replies.

use regex::Regex;
use serde::{Deserialize, Serialize};

use super::identity::ResolvedWorkIdentity;

pub const MAX_FAILURE_CHARS: usize = 4_000;
pub const MAX_STEP_CHARS: usize = 200;
pub const MAX_EVIDENCE_ITEMS: usize = 20;
pub const MAX_EVIDENCE_CHARS: usize = 1_000;
pub const MAX_PERMITTED_RETRIES: usize = 32;
const DEFAULT_DECISION_ACTION: &str =
    "Review the retained recovery evidence and decide the next step.";

/// The only classifications a diagnostic model may return.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosisOutcome {
    Resolved,
    Retryable,
    Blocked,
    NeedsDecision,
}

/// Failure information sent to a diagnostic session after secret redaction
/// and bounded truncation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureContext {
    pub current_step: String,
    pub failure: String,
    pub prior_evidence: Vec<String>,
}

impl FailureContext {
    pub fn new(
        current_step: impl AsRef<str>,
        failure: impl AsRef<str>,
        evidence: &[String],
    ) -> Self {
        Self {
            current_step: truncate_chars(&sanitize_text(current_step.as_ref()), MAX_STEP_CHARS),
            failure: truncate_chars(&sanitize_text(failure.as_ref()), MAX_FAILURE_CHARS),
            prior_evidence: evidence
                .iter()
                .take(MAX_EVIDENCE_ITEMS)
                .map(|item| truncate_chars(&sanitize_text(item), MAX_EVIDENCE_CHARS))
                .collect(),
        }
    }
}

/// A retry that the owning workflow explicitly permits. The diagnostic
/// model can select only one of these keys. It cannot invent an action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafeRetrySpec {
    pub key: String,
    pub description: String,
}

impl SafeRetrySpec {
    pub fn new(key: impl AsRef<str>, description: impl AsRef<str>) -> Result<Self, String> {
        let key = key.as_ref().trim();
        if key.is_empty()
            || key.len() > 64
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(format!("invalid safe retry key: {key}"));
        }
        Ok(Self {
            key: key.to_string(),
            description: truncate_chars(&sanitize_text(description.as_ref()), MAX_EVIDENCE_CHARS),
        })
    }
}

/// The JSON contract returned by a diagnostic session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticResponse {
    pub outcome: DiagnosisOutcome,
    pub summary: String,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default)]
    pub retry_key: Option<String>,
    #[serde(default)]
    pub question: Option<String>,
    #[serde(default)]
    pub requested_action: Option<String>,
}

impl DiagnosticResponse {
    pub fn needs_decision(summary: impl AsRef<str>, question: impl AsRef<str>) -> Self {
        Self {
            outcome: DiagnosisOutcome::NeedsDecision,
            summary: sanitize_text(summary.as_ref()),
            evidence: Vec::new(),
            retry_key: None,
            question: Some(sanitize_text(question.as_ref())),
            requested_action: Some(DEFAULT_DECISION_ACTION.to_string()),
        }
    }

    fn validate_and_sanitize(mut self) -> Result<Self, String> {
        self.summary = truncate_chars(&sanitize_text(&self.summary), MAX_EVIDENCE_CHARS);
        if self.summary.is_empty() {
            return Err("diagnostic response summary is empty".to_string());
        }
        self.evidence = self
            .evidence
            .into_iter()
            .take(MAX_EVIDENCE_ITEMS)
            .map(|item| truncate_chars(&sanitize_text(&item), MAX_EVIDENCE_CHARS))
            .filter(|item| !item.is_empty())
            .collect();
        self.retry_key = self
            .retry_key
            .map(|key| truncate_chars(&sanitize_text(&key), 64));
        self.question = self
            .question
            .map(|question| truncate_chars(&sanitize_text(&question), MAX_EVIDENCE_CHARS));
        self.requested_action = self
            .requested_action
            .map(|action| truncate_chars(&sanitize_text(&action), MAX_EVIDENCE_CHARS));
        if matches!(
            self.outcome,
            DiagnosisOutcome::Blocked | DiagnosisOutcome::NeedsDecision
        ) && self
            .requested_action
            .as_deref()
            .unwrap_or_default()
            .is_empty()
        {
            self.requested_action = Some(DEFAULT_DECISION_ACTION.to_string());
        }

        match self.outcome {
            DiagnosisOutcome::Retryable
                if self.retry_key.as_deref().unwrap_or_default().is_empty() =>
            {
                Err("retryable diagnostic response has no retry_key".to_string())
            }
            DiagnosisOutcome::Blocked | DiagnosisOutcome::NeedsDecision
                if self.question.as_deref().unwrap_or_default().is_empty() =>
            {
                Err("blocked diagnostic response has no question".to_string())
            }
            _ => Ok(self),
        }
    }
}

/// Parse a diagnostic reply. Markdown fences are accepted only as a transport
/// wrapper. The payload itself must match the closed JSON contract above.
pub fn parse_diagnostic_response(text: &str) -> Result<DiagnosticResponse, String> {
    let trimmed = text.trim();
    let payload = trimmed
        .strip_prefix("```json")
        .and_then(|value| value.strip_suffix("```"))
        .or_else(|| {
            trimmed
                .strip_prefix("```")
                .and_then(|value| value.strip_suffix("```"))
        })
        .map(str::trim)
        .unwrap_or(trimmed);
    serde_json::from_str::<DiagnosticResponse>(payload)
        .map_err(|error| format!("diagnostic response is not valid JSON: {error}"))?
        .validate_and_sanitize()
}

/// Render the exact diagnostic input. It contains no raw failure text,
/// process path, credential, or unrelated conversation context.
pub fn render_diagnostic_prompt(
    identity: &ResolvedWorkIdentity,
    failure: &FailureContext,
    permitted_retries: &[SafeRetrySpec],
) -> String {
    let failure = FailureContext::new(
        &failure.current_step,
        &failure.failure,
        &failure.prior_evidence,
    );
    let mut prompt = format!(
        "You are a diagnostic-only recovery assistant. Do not call tools and do not mutate files, repositories, or provider state. Return JSON only with keys outcome, summary, evidence, retry_key, question, and requested_action. outcome must be one of resolved, retryable, blocked, or needs_decision. A retryable response may select only a listed safe retry key.\n\nprovider: {}\nrepository: {}/{}\nproject: {}\nitem: {}#{}\ncurrent_step: {}\nsanitized_failure: {}\nprior_recovery_evidence:\n",
        identity.provider,
        identity.repository.namespace,
        identity.repository.name,
        identity.project.key,
        match identity.item.kind {
            super::identity::WorkItemKind::Issue => "issue",
            super::identity::WorkItemKind::PullRequest => "pull_request",
        },
        identity.item.number,
        failure.current_step,
        failure.failure,
    );

    if failure.prior_evidence.is_empty() {
        prompt.push_str("- none\n");
    } else {
        for evidence in &failure.prior_evidence {
            prompt.push_str("- ");
            prompt.push_str(evidence);
            prompt.push('\n');
        }
    }

    prompt.push_str("permitted_safe_retry_keys:\n");
    if permitted_retries.is_empty() {
        prompt.push_str("- none\n");
    } else {
        for retry in permitted_retries.iter().take(MAX_PERMITTED_RETRIES) {
            let key = truncate_chars(&sanitize_text(&retry.key), 64);
            let description =
                truncate_chars(&sanitize_text(&retry.description), MAX_EVIDENCE_CHARS);
            prompt.push_str("- ");
            prompt.push_str(&key);
            prompt.push_str(": ");
            prompt.push_str(&description);
            prompt.push('\n');
        }
        if permitted_retries.len() > MAX_PERMITTED_RETRIES {
            prompt.push_str("- additional retry choices were omitted by the recovery bound\n");
        }
    }
    prompt.push_str("If the evidence is insufficient, use needs_decision and state the missing fact in question.");
    prompt
}

/// Redact common credentials before a failure can enter a prompt or be
/// retained as recovery evidence.
pub fn sanitize_text(input: &str) -> String {
    let mut value = input.to_string();
    let json_credential = Regex::new(
        r#"(?i)([\"']?(?:api[_-]?key|token|password|secret)[\"']?\s*:\s*[\"']?)[^\"'\s,;}]+"#,
    )
    .expect("JSON credential redaction pattern is valid");
    value = json_credential
        .replace_all(&value, "$1[REDACTED]")
        .into_owned();

    let credential = Regex::new(
        r"(?i)(authorization\s*:\s*bearer\s+|(?:api[_-]?key|token|password|secret)\s*[=:]\s*)[^\s,;]+",
    )
    .expect("credential redaction pattern is valid");
    value = credential.replace_all(&value, "$1[REDACTED]").into_owned();

    let url_credentials =
        Regex::new(r"(?i)(https?://)[^/@\s]+:[^/@\s]+@").expect("URL redaction pattern is valid");
    value = url_credentials
        .replace_all(&value, "$1[REDACTED]@")
        .into_owned();

    let windows_path =
        Regex::new(r"(?i)\b[A-Z]:\\[^\s,;]+").expect("Windows path redaction pattern is valid");
    value = windows_path
        .replace_all(&value, "[PATH_REDACTED]")
        .into_owned();

    let unc_path = Regex::new(r"(?i)\\\\[^\s,;]+(?:\\[^\s,;]+)+")
        .expect("UNC path redaction pattern is valid");
    value = unc_path.replace_all(&value, "[PATH_REDACTED]").into_owned();

    let token = Regex::new(
        r"(?i)\b(?:sk-[A-Za-z0-9_-]+|ghp_[A-Za-z0-9_]+|github_pat_[A-Za-z0-9_]+|glpat-[A-Za-z0-9_-]+)\b",
    )
    .expect("token redaction pattern is valid");
    token.replace_all(&value, "[REDACTED]").into_owned()
}

fn truncate_chars(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    let mut output: String = value.chars().take(limit.saturating_sub(1)).collect();
    output.push('\u{2026}');
    output
}
