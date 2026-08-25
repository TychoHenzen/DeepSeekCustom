//! Promotion targets and the concurrent-edit baseline gate.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    BoundaryValidatedPatch, ProcedureFingerprintError, ProcedurePathFingerprint,
    capture_path_fingerprints,
};

/// The file operation represented by one promotion target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromotionTargetKind {
    Create,
    Update,
    Delete,
    Rename,
}

/// One typed file operation that a verified patch may promote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PromotionTarget {
    Create { path: String },
    Update { path: String },
    Delete { path: String },
    Rename { from: String, to: String },
}

impl PromotionTarget {
    pub const fn kind(&self) -> PromotionTargetKind {
        match self {
            Self::Create { .. } => PromotionTargetKind::Create,
            Self::Update { .. } => PromotionTargetKind::Update,
            Self::Delete { .. } => PromotionTargetKind::Delete,
            Self::Rename { .. } => PromotionTargetKind::Rename,
        }
    }

    /// Return every real path whose baseline must be checked.
    pub fn paths(&self) -> Vec<&str> {
        match self {
            Self::Create { path } | Self::Update { path } | Self::Delete { path } => {
                vec![path]
            }
            Self::Rename { from, to } => vec![from, to],
        }
    }
}

/// Failure to model a validated patch's file operations.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PromotionTargetError {
    #[error("promotion patch section {section} is malformed: {reason}")]
    Malformed { section: usize, reason: String },
}

/// Model every create, update, delete, and rename section in a patch.
pub fn model_promotion_targets(
    patch: &BoundaryValidatedPatch,
) -> Result<Vec<PromotionTarget>, PromotionTargetError> {
    let lines = patch.envelope().unified_diff.lines().collect::<Vec<_>>();
    let starts = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| line.starts_with("diff --git ").then_some(index))
        .collect::<Vec<_>>();
    if starts.is_empty() {
        return Err(malformed(1, "no diff sections were found"));
    }

    starts
        .iter()
        .enumerate()
        .map(|(index, start)| {
            let end = starts.get(index + 1).copied().unwrap_or(lines.len());
            model_section(index + 1, &lines[*start..end])
        })
        .collect()
}

/// A captured preview baseline for every real endpoint in a promotion plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromotionBaseline {
    fingerprints: Vec<ProcedurePathFingerprint>,
}

impl PromotionBaseline {
    /// Capture the current real-workspace state for every target endpoint.
    pub fn capture(
        project_root: &Path,
        targets: &[PromotionTarget],
    ) -> Result<Self, ProcedureFingerprintError> {
        let paths = targets
            .iter()
            .flat_map(PromotionTarget::paths)
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        let fingerprints = capture_path_fingerprints(project_root, paths.into_iter())?;
        Ok(Self { fingerprints })
    }

    /// Build a baseline from previously captured preview fingerprints.
    pub fn from_fingerprints(fingerprints: Vec<ProcedurePathFingerprint>) -> Self {
        Self { fingerprints }
    }

    pub fn fingerprints(&self) -> &[ProcedurePathFingerprint] {
        &self.fingerprints
    }

    /// Compare every baseline endpoint with the current real workspace.
    pub fn compare(
        &self,
        project_root: &Path,
    ) -> Result<PromotionBaselineComparison, ProcedureFingerprintError> {
        let current = capture_path_fingerprints(
            project_root,
            self.fingerprints
                .iter()
                .map(|fingerprint| fingerprint.path.clone()),
        )?;
        let current_by_path = current
            .into_iter()
            .map(|fingerprint| (fingerprint.path.clone(), fingerprint))
            .collect::<BTreeMap<_, _>>();
        let stale_paths = self
            .fingerprints
            .iter()
            .filter_map(|expected| {
                let actual = current_by_path
                    .get(&expected.path)
                    .expect("current fingerprints contain every baseline path");
                (actual != expected).then(|| StalePromotionPath {
                    path: expected.path.clone(),
                    expected: expected.clone(),
                    actual: actual.clone(),
                })
            })
            .collect();
        Ok(PromotionBaselineComparison {
            checked_paths: self
                .fingerprints
                .iter()
                .map(|fingerprint| fingerprint.path.clone())
                .collect(),
            stale_paths,
        })
    }

    /// Refuse the promotion gate when any real endpoint changed.
    pub fn ensure_current(
        &self,
        project_root: &Path,
    ) -> Result<PromotionBaselineComparison, PromotionBaselineCheckError> {
        let comparison = self.compare(project_root)?;
        if !comparison.can_promote() {
            return Err(PromotionBaselineCheckError::Stale {
                stale_paths: comparison.stale_paths.clone(),
            });
        }
        Ok(comparison)
    }
}

/// One real path whose state differs from the preview baseline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StalePromotionPath {
    pub path: String,
    pub expected: ProcedurePathFingerprint,
    pub actual: ProcedurePathFingerprint,
}

/// Result of the immediate pre-promotion baseline comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromotionBaselineComparison {
    pub checked_paths: Vec<String>,
    pub stale_paths: Vec<StalePromotionPath>,
}

impl PromotionBaselineComparison {
    pub fn can_promote(&self) -> bool {
        self.stale_paths.is_empty()
    }

    pub fn is_current(&self) -> bool {
        self.can_promote()
    }
}

/// Failure of the pre-promotion baseline gate.
#[derive(Debug, Error)]
pub enum PromotionBaselineCheckError {
    #[error(transparent)]
    Fingerprint(#[from] ProcedureFingerprintError),
    #[error("promotion baseline is stale for paths: {stale_paths:?}")]
    Stale {
        stale_paths: Vec<StalePromotionPath>,
    },
}

#[derive(Clone, Copy)]
enum EndpointSide {
    Old,
    New,
    Rename,
}

fn model_section(section: usize, lines: &[&str]) -> Result<PromotionTarget, PromotionTargetError> {
    let header = lines
        .first()
        .and_then(|line| line.strip_prefix("diff --git "))
        .ok_or_else(|| malformed(section, "missing diff header"))?;
    let (old, new) = parse_diff_header(header).map_err(|reason| malformed(section, reason))?;
    let old_header = matching_header(lines, "--- ", section)?;
    let new_header = matching_header(lines, "+++ ", section)?;
    let old_is_missing = old_header.as_deref().is_some_and(is_dev_null);
    let new_is_missing = new_header.as_deref().is_some_and(is_dev_null);
    let rename_from = matching_header(lines, "rename from ", section)?
        .map(|value| required_endpoint(&value, EndpointSide::Rename))
        .transpose()
        .map_err(|reason| malformed(section, reason))?;
    let rename_to = matching_header(lines, "rename to ", section)?
        .map(|value| required_endpoint(&value, EndpointSide::Rename))
        .transpose()
        .map_err(|reason| malformed(section, reason))?;

    if rename_from.is_some() != rename_to.is_some() {
        return Err(malformed(
            section,
            "rename source and destination must be paired",
        ));
    }
    if let (Some(from), Some(to)) = (rename_from, rename_to) {
        if old.as_deref() != Some(from.as_str()) || new.as_deref() != Some(to.as_str()) {
            return Err(malformed(
                section,
                "rename metadata does not match the diff endpoints",
            ));
        }
        return Ok(PromotionTarget::Rename { from, to });
    }

    match (old_is_missing, new_is_missing, old, new) {
        (true, false, _, Some(path)) => Ok(PromotionTarget::Create { path }),
        (false, true, Some(path), _) => Ok(PromotionTarget::Delete { path }),
        (false, false, Some(old), Some(new)) if old == new => {
            Ok(PromotionTarget::Update { path: new })
        }
        _ => Err(malformed(
            section,
            "file endpoints do not describe a create, update, delete, or rename",
        )),
    }
}

fn matching_header(
    lines: &[&str],
    prefix: &str,
    section: usize,
) -> Result<Option<String>, PromotionTargetError> {
    let matches = lines
        .iter()
        .filter_map(|line| line.strip_prefix(prefix))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Ok(None),
        [value] => Ok(Some(value.trim().to_owned())),
        _ => Err(malformed(
            section,
            format!("found more than one `{prefix}` header"),
        )),
    }
}

fn parse_diff_header(header: &str) -> Result<(Option<String>, Option<String>), String> {
    let (old, rest) = parse_path_token(header)?;
    let (new, trailing) = parse_path_token(rest.trim_start())?;
    if !trailing.trim().is_empty() {
        return Err("unexpected content after the new endpoint".to_string());
    }
    Ok((
        normalize_endpoint(&old, EndpointSide::Old, true)?,
        normalize_endpoint(&new, EndpointSide::New, true)?,
    ))
}

fn parse_path_token(input: &str) -> Result<(String, &str), String> {
    if let Some(quoted) = input.strip_prefix('"') {
        let mut value = String::new();
        let mut escaped = false;
        for (index, character) in quoted.char_indices() {
            if escaped {
                let decoded = match character {
                    '"' => '"',
                    '\\' => '\\',
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    _ => return Err(format!("unsupported quoted-path escape `\\{character}`")),
                };
                value.push(decoded);
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                return Ok((value, &quoted[index + character.len_utf8()..]));
            } else {
                value.push(character);
            }
        }
        return Err("unterminated quoted path".to_string());
    }

    let input = input.trim_start();
    let end = input.find(char::is_whitespace).unwrap_or(input.len());
    if end == 0 {
        return Err("path endpoint is empty".to_string());
    }
    Ok((input[..end].to_string(), &input[end..]))
}

fn normalize_endpoint(
    raw: &str,
    side: EndpointSide,
    allow_dev_null: bool,
) -> Result<Option<String>, String> {
    let (parsed, trailing) = parse_path_token(raw.trim())?;
    if !trailing.trim().is_empty() {
        return Err("unexpected content after the path endpoint".to_string());
    }
    let replaced = parsed.replace('\\', "/");
    if allow_dev_null && replaced == "/dev/null" {
        return Ok(None);
    }
    let stripped = match side {
        EndpointSide::Old => replaced.strip_prefix("a/").unwrap_or(&replaced),
        EndpointSide::New => replaced.strip_prefix("b/").unwrap_or(&replaced),
        EndpointSide::Rename => &replaced,
    };
    if stripped.is_empty() || stripped.starts_with('/') || stripped.starts_with("//") {
        return Err(format!("invalid repository path `{parsed}`"));
    }
    let parts = stripped.split('/').collect::<Vec<_>>();
    if parts
        .iter()
        .any(|part| part.is_empty() || *part == "." || *part == "..")
    {
        return Err(format!("repository path is not normalized: `{parsed}`"));
    }
    Ok(Some(parts.join("/")))
}

fn required_endpoint(raw: &str, side: EndpointSide) -> Result<String, String> {
    normalize_endpoint(raw, side, false)?.ok_or_else(|| "endpoint cannot be /dev/null".to_string())
}

fn is_dev_null(raw: &str) -> bool {
    raw.trim().trim_matches('"').replace('\\', "/") == "/dev/null"
}

fn malformed(section: usize, reason: impl Into<String>) -> PromotionTargetError {
    PromotionTargetError::Malformed {
        section,
        reason: reason.into(),
    }
}
