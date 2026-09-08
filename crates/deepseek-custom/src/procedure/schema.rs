//! Deterministic structured-output schema for Stage 1 localization.

use crate::api::types::{JsonSchemaFormat, ResponseFormat};

use super::RepositoryIndexEntry;

/// Build the Ollama structured-response field for the current repository.
/// Valid paths are an enum. Symbol membership remains a post-decode check
/// because a per-path symbol schema would make the request much larger.
pub fn localization_response_format(index: &[RepositoryIndexEntry]) -> ResponseFormat {
    let paths = index
        .iter()
        .map(|entry| serde_json::Value::String(entry.path.clone()))
        .collect::<Vec<_>>();
    ResponseFormat::JsonSchema {
        json_schema: JsonSchemaFormat {
            name: "procedure_localization".to_string(),
            strict: true,
            schema: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["targets"],
                "properties": {
                    "targets": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["path", "evidence"],
                            "properties": {
                                "path": { "type": "string", "enum": paths },
                                "symbol": { "type": ["string", "null"] },
                                "evidence": { "type": "string", "minLength": 1 }
                            }
                        }
                    }
                }
            }),
        },
    }
}
