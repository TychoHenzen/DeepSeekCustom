//! Localization-boundary validation for structurally decoded patches.

use std::collections::BTreeSet;
use std::fmt;

use super::{PatchCandidate, PatchEnvelope};

/// A patch candidate whose every diff endpoint is in the localization allowlist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundaryValidatedPatch {
    envelope: PatchEnvelope,
    file_count: usize,
    paths: Vec<String>,
}

impl BoundaryValidatedPatch {
    pub fn envelope(&self) -> &PatchEnvelope {
        &self.envelope
    }

    pub fn into_envelope(self) -> PatchEnvelope {
        self.envelope
    }

    pub fn file_count(&self) -> usize {
        self.file_count
    }

    /// Sorted, duplicate-free repository paths observed in the diff.
    pub fn paths(&self) -> &[String] {
        &self.paths
    }
}

/// All deterministic reasons that a patch cannot cross the localization gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchBoundaryError {
    violations: Vec<String>,
}

impl PatchBoundaryError {
    pub fn violations(&self) -> &[String] {
        &self.violations
    }
}

impl fmt::Display for PatchBoundaryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "patch violates the localization boundary:")?;
        for violation in &self.violations {
            write!(formatter, "\n- {violation}")?;
        }
        Ok(())
    }
}

impl std::error::Error for PatchBoundaryError {}

/// Consume a structurally decoded candidate and authorize every diff endpoint.
///
/// The returned type is the only patch type that represents preview eligibility.
pub fn validate_patch_boundary<I, S>(
    candidate: PatchCandidate,
    localization_allowlist: I,
) -> Result<BoundaryValidatedPatch, PatchBoundaryError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let allowlist = localization_allowlist
        .into_iter()
        .map(|path| path.as_ref().replace('\\', "/"))
        .collect::<BTreeSet<_>>();
    let file_count = candidate.file_count();
    let envelope = candidate.into_envelope();
    let mut paths = BTreeSet::new();
    let mut violations = BTreeSet::new();

    collect_diff_paths(&envelope.unified_diff, &mut paths, &mut violations);
    for path in &paths {
        if !allowlist.contains(path) {
            violations.insert(format!(
                "unexpected path `{path}` is outside the localization allowlist"
            ));
        }
    }

    if !violations.is_empty() {
        return Err(PatchBoundaryError {
            violations: violations.into_iter().collect(),
        });
    }

    Ok(BoundaryValidatedPatch {
        envelope,
        file_count,
        paths: paths.into_iter().collect(),
    })
}

#[derive(Clone, Copy)]
enum EndpointSide {
    Old,
    New,
    Rename,
}

impl EndpointSide {
    fn label(self) -> &'static str {
        match self {
            Self::Old => "old",
            Self::New => "new",
            Self::Rename => "rename",
        }
    }
}

#[derive(Default)]
struct SectionPaths {
    diff_old: Option<String>,
    diff_new: Option<String>,
    content_old: Option<String>,
    content_new: Option<String>,
    rename_old: Option<String>,
    rename_new: Option<String>,
    old_is_null: bool,
    new_is_null: bool,
}

fn collect_diff_paths(diff: &str, paths: &mut BTreeSet<String>, violations: &mut BTreeSet<String>) {
    let lines = diff.lines().collect::<Vec<_>>();
    let mut cursor = 0;
    while cursor < lines.len() {
        let section_line = cursor + 1;
        let section_start = cursor;
        cursor += 1;
        while cursor < lines.len() && !lines[cursor].starts_with("diff --git ") {
            cursor += 1;
        }
        collect_section_paths(
            &lines[section_start..cursor],
            section_line,
            paths,
            violations,
        );
    }
}

fn collect_section_paths(
    lines: &[&str],
    first_line: usize,
    paths: &mut BTreeSet<String>,
    violations: &mut BTreeSet<String>,
) {
    let mut section = SectionPaths::default();
    let diff_header = lines[0].strip_prefix("diff --git ").unwrap_or_default();
    match parse_diff_header(diff_header) {
        Ok((old, new)) => {
            section.diff_old =
                normalize_endpoint(&old, EndpointSide::Old, false, first_line, violations);
            section.diff_new =
                normalize_endpoint(&new, EndpointSide::New, false, first_line, violations);
        }
        Err(reason) => {
            violations.insert(format!(
                "malformed diff header at line {first_line}: {reason}"
            ));
        }
    }

    let old_headers = matching_lines(lines, "--- ");
    let new_headers = matching_lines(lines, "+++ ");
    validate_pair_count("file", &old_headers, &new_headers, first_line, violations);
    if let Some((index, raw)) = old_headers.first() {
        section.old_is_null = is_dev_null(raw);
        section.content_old =
            normalize_endpoint(raw, EndpointSide::Old, true, first_line + index, violations);
    }
    if let Some((index, raw)) = new_headers.first() {
        section.new_is_null = is_dev_null(raw);
        section.content_new =
            normalize_endpoint(raw, EndpointSide::New, true, first_line + index, violations);
    }

    let rename_from = matching_lines(lines, "rename from ");
    let rename_to = matching_lines(lines, "rename to ");
    validate_pair_count("rename", &rename_from, &rename_to, first_line, violations);
    if let Some((index, raw)) = rename_from.first() {
        section.rename_old = normalize_endpoint(
            raw,
            EndpointSide::Rename,
            false,
            first_line + index,
            violations,
        );
    }
    if let Some((index, raw)) = rename_to.first() {
        section.rename_new = normalize_endpoint(
            raw,
            EndpointSide::Rename,
            false,
            first_line + index,
            violations,
        );
    }

    validate_endpoint_matches(&section, first_line, violations);
    paths.extend(
        [
            section.diff_old,
            section.diff_new,
            section.content_old,
            section.content_new,
            section.rename_old,
            section.rename_new,
        ]
        .into_iter()
        .flatten(),
    );
}

fn matching_lines<'a>(lines: &'a [&str], prefix: &str) -> Vec<(usize, &'a str)> {
    lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| line.strip_prefix(prefix).map(|value| (index, value)))
        .collect()
}

fn validate_pair_count(
    kind: &str,
    old: &[(usize, &str)],
    new: &[(usize, &str)],
    first_line: usize,
    violations: &mut BTreeSet<String>,
) {
    if old.len() > 1 {
        violations.insert(format!(
            "malformed {kind} headers in section at line {first_line}: found {} old endpoints",
            old.len()
        ));
    }
    if new.len() > 1 {
        violations.insert(format!(
            "malformed {kind} headers in section at line {first_line}: found {} new endpoints",
            new.len()
        ));
    }
    if old.is_empty() != new.is_empty() {
        violations.insert(format!(
            "malformed {kind} headers in section at line {first_line}: old and new endpoints must be paired"
        ));
    }
}

fn validate_endpoint_matches(
    section: &SectionPaths,
    first_line: usize,
    violations: &mut BTreeSet<String>,
) {
    compare_endpoint(
        "old file header",
        section.diff_old.as_deref(),
        section.content_old.as_deref(),
        first_line,
        violations,
    );
    compare_endpoint(
        "new file header",
        section.diff_new.as_deref(),
        section.content_new.as_deref(),
        first_line,
        violations,
    );
    compare_endpoint(
        "rename source",
        section.diff_old.as_deref(),
        section.rename_old.as_deref(),
        first_line,
        violations,
    );
    compare_endpoint(
        "rename destination",
        section.diff_new.as_deref(),
        section.rename_new.as_deref(),
        first_line,
        violations,
    );

    if section.old_is_null {
        compare_endpoint(
            "create destination",
            section.diff_old.as_deref(),
            section.diff_new.as_deref(),
            first_line,
            violations,
        );
    }
    if section.new_is_null {
        compare_endpoint(
            "delete source",
            section.diff_new.as_deref(),
            section.diff_old.as_deref(),
            first_line,
            violations,
        );
    }
}

fn compare_endpoint(
    label: &str,
    declared: Option<&str>,
    actual: Option<&str>,
    first_line: usize,
    violations: &mut BTreeSet<String>,
) {
    if let (Some(declared), Some(actual)) = (declared, actual)
        && declared != actual
    {
        violations.insert(format!(
            "mismatched {label} in section at line {first_line}: diff header has `{declared}`, metadata has `{actual}`"
        ));
    }
}

fn parse_diff_header(header: &str) -> Result<(String, String), String> {
    let (old, rest) = parse_path_token(header)?;
    let rest = rest.trim_start();
    if rest.is_empty() {
        return Err("expected two path endpoints".to_string());
    }
    let (new, trailing) = parse_path_token(rest)?;
    if !trailing.trim().is_empty() {
        return Err("unexpected content after the new endpoint".to_string());
    }
    Ok((old, new))
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
    line: usize,
    violations: &mut BTreeSet<String>,
) -> Option<String> {
    let raw = raw.trim();
    let parsed = if raw.starts_with('"') {
        match parse_path_token(raw) {
            Ok((path, trailing)) if trailing.trim().is_empty() => path,
            Ok(_) => {
                violations.insert(format!(
                    "malformed {} path at line {line}: unexpected content after the endpoint",
                    side.label()
                ));
                return None;
            }
            Err(reason) => {
                violations.insert(format!(
                    "malformed {} path at line {line}: {reason}",
                    side.label()
                ));
                return None;
            }
        }
    } else {
        raw.to_string()
    };
    let replaced = parsed.replace('\\', "/");
    if allow_dev_null && replaced == "/dev/null" {
        return None;
    }
    let stripped = match side {
        EndpointSide::Old => replaced.strip_prefix("a/").unwrap_or(&replaced),
        EndpointSide::New => replaced.strip_prefix("b/").unwrap_or(&replaced),
        EndpointSide::Rename => &replaced,
    };
    match normalize_repository_path(stripped) {
        Ok(path) => Some(path),
        Err(reason) => {
            violations.insert(format!(
                "invalid {} path `{parsed}` at line {line}: {reason}",
                side.label()
            ));
            None
        }
    }
}

fn normalize_repository_path(path: &str) -> Result<String, &'static str> {
    if path.is_empty() {
        return Err("path is empty");
    }
    let bytes = path.as_bytes();
    if path.starts_with('/')
        || path.starts_with("//")
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
    {
        return Err("absolute paths are not allowed");
    }

    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => return Err("path is not normalized"),
            ".." => return Err("path traversal is not allowed"),
            _ => parts.push(part),
        }
    }
    Ok(parts.join("/"))
}

fn is_dev_null(raw: &str) -> bool {
    raw.trim().trim_matches('"').replace('\\', "/") == "/dev/null"
}
