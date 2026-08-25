//! Backend-neutral patch output and deterministic decoding.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::api::types::{JsonSchemaFormat, ResponseFormat};

use super::{RouteDecision, RouteOverride, RouteSignal, RouteTier};

/// Route evidence repeated in a model-produced patch envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchRouteMetadata {
    pub automatic_tier: RouteTier,
    pub effective_tier: RouteTier,
    pub signals: Vec<RouteSignal>,
    pub selected_override: RouteOverride,
    pub overridden: bool,
}

impl From<RouteDecision> for PatchRouteMetadata {
    fn from(decision: RouteDecision) -> Self {
        Self {
            automatic_tier: decision.automatic_tier,
            effective_tier: decision.effective_tier,
            signals: decision.signals,
            selected_override: decision.selected_override,
            overridden: decision.overridden,
        }
    }
}

/// The single output shape accepted from every patch-drafting backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchEnvelope {
    pub targets: Vec<String>,
    pub rationale: String,
    pub route: PatchRouteMetadata,
    pub unified_diff: String,
}

/// One structurally validated patch envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchCandidate {
    envelope: PatchEnvelope,
    file_count: usize,
}

impl PatchCandidate {
    pub fn envelope(&self) -> &PatchEnvelope {
        &self.envelope
    }

    pub fn into_envelope(self) -> PatchEnvelope {
        self.envelope
    }

    pub fn file_count(&self) -> usize {
        self.file_count
    }
}

/// A deterministic failure from the shared envelope and diff decoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchEnvelopeError {
    Json {
        message: String,
        line: usize,
        column: usize,
    },
    Structure {
        reason: String,
    },
    Diff {
        reason: String,
    },
}

impl fmt::Display for PatchEnvelopeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json {
                message,
                line,
                column,
            } => write!(
                formatter,
                "patch output is not exactly one JSON document: {message} at line {line} column {column}"
            ),
            Self::Structure { reason } => {
                write!(
                    formatter,
                    "patch envelope is structurally invalid: {reason}"
                )
            }
            Self::Diff { reason } => {
                write!(
                    formatter,
                    "patch envelope unified_diff is invalid: {reason}"
                )
            }
        }
    }
}

impl std::error::Error for PatchEnvelopeError {}

/// Decode exactly one JSON document and validate its unified diff.
pub fn decode_patch_envelope(output: &str) -> Result<PatchCandidate, PatchEnvelopeError> {
    let value = serde_json::from_str::<serde_json::Value>(output).map_err(json_error)?;
    reject_extra_route_signal_fields(&value)?;
    let envelope = serde_json::from_value::<PatchEnvelope>(value).map_err(|error| {
        PatchEnvelopeError::Structure {
            reason: error.to_string(),
        }
    })?;
    validate_envelope_fields(&envelope)?;
    let file_count = validate_unified_diff(&envelope.unified_diff)?;
    Ok(PatchCandidate {
        envelope,
        file_count,
    })
}

fn reject_extra_route_signal_fields(value: &serde_json::Value) -> Result<(), PatchEnvelopeError> {
    let Some(signals) = value
        .get("route")
        .and_then(|route| route.get("signals"))
        .and_then(serde_json::Value::as_array)
    else {
        return Ok(());
    };
    for (index, signal) in signals.iter().enumerate() {
        let Some(object) = signal.as_object() else {
            continue;
        };
        let kind = object
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let accepts_value = matches!(kind, "mechanical_verb" | "target_count");
        for key in object.keys() {
            if key != "kind" && !(accepts_value && key == "value") {
                return Err(structure_error(format!(
                    "unknown field `{key}` in `route.signals[{index}]`"
                )));
            }
        }
    }
    Ok(())
}

/// The strict provider response shape used for local patch drafting.
pub fn patch_envelope_response_format() -> ResponseFormat {
    ResponseFormat::JsonSchema {
        json_schema: JsonSchemaFormat {
            name: "procedure_patch_envelope".to_string(),
            strict: true,
            schema: patch_envelope_schema(),
        },
    }
}

fn patch_envelope_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["targets", "rationale", "route", "unified_diff"],
        "properties": {
            "targets": {
                "type": "array",
                "minItems": 1,
                "uniqueItems": true,
                "items": { "type": "string", "minLength": 1 }
            },
            "rationale": { "type": "string", "minLength": 1 },
            "route": {
                "type": "object",
                "additionalProperties": false,
                "required": [
                    "automatic_tier",
                    "effective_tier",
                    "signals",
                    "selected_override",
                    "overridden"
                ],
                "properties": {
                    "automatic_tier": { "type": "string", "enum": ["local", "frontier"] },
                    "effective_tier": { "type": "string", "enum": ["local", "frontier"] },
                    "signals": {
                        "type": "array",
                        "items": { "$ref": "#/$defs/route_signal" }
                    },
                    "selected_override": {
                        "type": "string",
                        "enum": ["automatic", "force_local", "force_frontier"]
                    },
                    "overridden": { "type": "boolean" }
                }
            },
            "unified_diff": { "type": "string", "minLength": 1 }
        },
        "$defs": {
            "route_signal": {
                "oneOf": [
                    {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["kind", "value"],
                        "properties": {
                            "kind": { "const": "mechanical_verb" },
                            "value": {
                                "type": "string",
                                "enum": [
                                    "rename",
                                    "import",
                                    "signature_propagation",
                                    "boilerplate",
                                    "test_scaffolding",
                                    "formatting",
                                    "documentation"
                                ]
                            }
                        }
                    },
                    {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["kind", "value"],
                        "properties": {
                            "kind": { "const": "target_count" },
                            "value": { "type": "integer", "minimum": 0 }
                        }
                    },
                    {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["kind"],
                        "properties": {
                            "kind": {
                                "type": "string",
                                "enum": [
                                    "architecture",
                                    "cross_cutting_behavior",
                                    "concurrency",
                                    "security",
                                    "migration",
                                    "public_api",
                                    "subtle_bug",
                                    "substantive_logic",
                                    "unknown_wording"
                                ]
                            }
                        }
                    }
                ]
            }
        }
    })
}

fn json_error(error: serde_json::Error) -> PatchEnvelopeError {
    PatchEnvelopeError::Json {
        message: error
            .to_string()
            .split(" at line ")
            .next()
            .unwrap_or_default()
            .to_string(),
        line: error.line(),
        column: error.column(),
    }
}

fn validate_envelope_fields(envelope: &PatchEnvelope) -> Result<(), PatchEnvelopeError> {
    if envelope.targets.is_empty() {
        return Err(structure_error(
            "field `targets` must contain at least one path",
        ));
    }
    if envelope.targets.iter().any(|path| path.trim().is_empty()) {
        return Err(structure_error("field `targets` contains an empty path"));
    }
    let mut targets = envelope.targets.clone();
    targets.sort();
    targets.dedup();
    if targets.len() != envelope.targets.len() {
        return Err(structure_error("field `targets` contains a duplicate path"));
    }
    if envelope.rationale.trim().is_empty() {
        return Err(structure_error("field `rationale` must not be empty"));
    }
    if envelope.route.overridden != (envelope.route.automatic_tier != envelope.route.effective_tier)
    {
        return Err(structure_error(
            "field `route.overridden` does not match the automatic and effective tiers",
        ));
    }
    let expected_effective = match envelope.route.selected_override {
        RouteOverride::Automatic => envelope.route.automatic_tier,
        RouteOverride::ForceLocal => RouteTier::Local,
        RouteOverride::ForceFrontier => RouteTier::Frontier,
    };
    if envelope.route.effective_tier != expected_effective {
        return Err(structure_error(
            "field `route.effective_tier` does not match `route.selected_override`",
        ));
    }
    Ok(())
}

fn structure_error(reason: impl Into<String>) -> PatchEnvelopeError {
    PatchEnvelopeError::Structure {
        reason: reason.into(),
    }
}

fn diff_error(reason: impl Into<String>) -> PatchEnvelopeError {
    PatchEnvelopeError::Diff {
        reason: reason.into(),
    }
}

fn validate_unified_diff(diff: &str) -> Result<usize, PatchEnvelopeError> {
    if diff.trim().is_empty() {
        return Err(diff_error("diff is empty"));
    }
    let lines = diff.lines().collect::<Vec<_>>();
    if !lines[0].starts_with("diff --git ") {
        return Err(diff_error("first line must start with `diff --git `"));
    }

    let mut cursor = 0;
    let mut files = 0;
    while cursor < lines.len() {
        if !lines[cursor].starts_with("diff --git ") {
            return Err(diff_error(format!(
                "line {} must start a file section with `diff --git `",
                cursor + 1
            )));
        }
        files += 1;
        cursor += 1;
        let section_start = cursor;
        while cursor < lines.len() && !lines[cursor].starts_with("diff --git ") {
            cursor += 1;
        }
        validate_file_section(&lines[section_start..cursor], section_start + 1)?;
    }
    Ok(files)
}

fn validate_file_section(
    lines: &[&str],
    first_line_number: usize,
) -> Result<(), PatchEnvelopeError> {
    let old_header = lines.iter().position(|line| line.starts_with("--- "));
    let rename_from = lines
        .iter()
        .position(|line| line.starts_with("rename from "));
    match (old_header, rename_from) {
        (Some(old_index), _) => validate_hunk_section(lines, first_line_number, old_index),
        (None, Some(from_index)) => validate_rename_section(lines, first_line_number, from_index),
        (None, None) => Err(diff_error(format!(
            "file section at line {} has neither `---`/`+++` headers nor rename metadata",
            first_line_number.saturating_sub(1)
        ))),
    }
}

fn validate_rename_section(
    lines: &[&str],
    first_line_number: usize,
    from_index: usize,
) -> Result<(), PatchEnvelopeError> {
    let to_index = lines.iter().position(|line| line.starts_with("rename to "));
    if to_index.is_none_or(|index| index <= from_index) {
        return Err(diff_error(format!(
            "rename at line {} has no following `rename to` line",
            first_line_number + from_index
        )));
    }
    Ok(())
}

fn validate_hunk_section(
    lines: &[&str],
    first_line_number: usize,
    old_index: usize,
) -> Result<(), PatchEnvelopeError> {
    let new_index = old_index + 1;
    if lines
        .get(new_index)
        .is_none_or(|line| !line.starts_with("+++ "))
    {
        return Err(diff_error(format!(
            "line {} must be followed by a `+++` header",
            first_line_number + old_index
        )));
    }
    let mut cursor = new_index + 1;
    let mut hunks = 0;
    while cursor < lines.len() {
        if !lines[cursor].starts_with("@@ ") {
            return Err(diff_error(format!(
                "line {} is outside a unified-diff hunk",
                first_line_number + cursor
            )));
        }
        let (expected_old, expected_new) = parse_hunk_header(lines[cursor]).ok_or_else(|| {
            diff_error(format!(
                "line {} has an invalid hunk header",
                first_line_number + cursor
            ))
        })?;
        cursor += 1;
        let mut actual_old = 0;
        let mut actual_new = 0;
        while cursor < lines.len() && !lines[cursor].starts_with("@@ ") {
            match lines[cursor].chars().next() {
                Some(' ') => {
                    actual_old += 1;
                    actual_new += 1;
                }
                Some('-') => actual_old += 1,
                Some('+') => actual_new += 1,
                Some('\\') if lines[cursor] == "\\ No newline at end of file" => {}
                _ => {
                    return Err(diff_error(format!(
                        "line {} has an invalid hunk prefix",
                        first_line_number + cursor
                    )));
                }
            }
            cursor += 1;
        }
        if (actual_old, actual_new) != (expected_old, expected_new) {
            return Err(diff_error(format!(
                "hunk ending before line {} declares {expected_old} old and {expected_new} new lines but contains {actual_old} old and {actual_new} new lines",
                first_line_number + cursor
            )));
        }
        hunks += 1;
    }
    if hunks == 0 {
        return Err(diff_error(format!(
            "file section at line {} has no hunk",
            first_line_number.saturating_sub(1)
        )));
    }
    Ok(())
}

fn parse_hunk_header(line: &str) -> Option<(usize, usize)> {
    let rest = line.strip_prefix("@@ -")?;
    let (old_range, rest) = rest.split_once(" +")?;
    let (new_range, _) = rest.split_once(" @@")?;
    Some((range_count(old_range)?, range_count(new_range)?))
}

fn range_count(range: &str) -> Option<usize> {
    let (start, count) = range.split_once(',').unwrap_or((range, "1"));
    start.parse::<usize>().ok()?;
    count.parse::<usize>().ok()
}
