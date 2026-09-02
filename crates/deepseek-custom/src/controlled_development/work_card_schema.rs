use super::work_card::{
    MAX_COMPLEXITY_EXCEPTIONS, MAX_EXCLUSIONS, MAX_PRODUCTION_PATHS, MAX_PROOF_COMMAND_CHARS,
    MAX_PROOF_COMMANDS, MAX_SUPPORTING_PATHS, MAX_WORK_CARD_ID_CHARS, MAX_WORK_CARD_ITEM_CHARS,
    MAX_WORK_CARD_OUTCOME_CHARS, MAX_WORK_CARD_PATH_CHARS,
};

/// Strict provider-facing JSON Schema for one complete Work Card response.
///
/// Semantic path and text checks still run through `WorkCard::validate` after
/// decoding. This closes the field set and provider-enforceable bounds first.
pub fn work_card_json_schema() -> serde_json::Value {
    let bounded_text = |maximum: usize| {
        serde_json::json!({
            "type": "string",
            "minLength": 1,
            "maxLength": maximum
        })
    };
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": [
            "id", "outcome", "proof_commands", "production_paths",
            "supporting_paths", "excluded", "complexity_exceptions"
        ],
        "properties": {
            "id": bounded_text(MAX_WORK_CARD_ID_CHARS),
            "outcome": bounded_text(MAX_WORK_CARD_OUTCOME_CHARS),
            "proof_commands": {
                "type": "array",
                "minItems": 1,
                "maxItems": MAX_PROOF_COMMANDS,
                "items": bounded_text(MAX_PROOF_COMMAND_CHARS)
            },
            "production_paths": {
                "type": "array",
                "maxItems": MAX_PRODUCTION_PATHS,
                "items": bounded_text(MAX_WORK_CARD_PATH_CHARS)
            },
            "supporting_paths": {
                "type": "array",
                "maxItems": MAX_SUPPORTING_PATHS,
                "items": bounded_text(MAX_WORK_CARD_PATH_CHARS)
            },
            "excluded": {
                "type": "array",
                "minItems": 1,
                "maxItems": MAX_EXCLUSIONS,
                "items": bounded_text(MAX_WORK_CARD_ITEM_CHARS)
            },
            "complexity_exceptions": {
                "type": "array",
                "maxItems": MAX_COMPLEXITY_EXCEPTIONS,
                "items": bounded_text(MAX_WORK_CARD_ITEM_CHARS)
            }
        }
    })
}
