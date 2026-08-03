use serde::Deserialize;

/// One selectable option for a question.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

/// One question posed by the `AskUserQuestion` tool.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Question {
    pub question: String,
    pub header: String,
    pub options: Vec<QuestionOption>,
    #[serde(rename = "multiSelect", default)]
    pub multi_select: bool,
}

/// The whole tool argument payload.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AskInput {
    pub questions: Vec<Question>,
}

/// One resolved answer: the labels chosen for one question.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    pub question: String,
    pub labels: Vec<String>,
}

/// The JSON Schema for `AskInput`, describing the shape the model must fill
/// in when calling the `AskUserQuestion` tool.
pub fn input_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "questions": {
                "type": "array",
                "description": "One or more questions to ask, each with its own set of options.",
                "items": {
                    "type": "object",
                    "properties": {
                        "question": {
                            "type": "string",
                            "description": "The full question text to present."
                        },
                        "header": {
                            "type": "string",
                            "description": "A short label for the question, shown as a heading."
                        },
                        "options": {
                            "type": "array",
                            "description": "The choices offered for this question. Label values must be unique within a question.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "label": {
                                        "type": "string",
                                        "description": "The short choice text, unique within this question's options."
                                    },
                                    "description": {
                                        "type": "string",
                                        "description": "A longer explanation of what choosing this option means."
                                    }
                                },
                                "required": ["label", "description"]
                            }
                        },
                        "multiSelect": {
                            "type": "boolean",
                            "description": "Whether more than one option may be chosen for this question. Defaults to false."
                        }
                    },
                    "required": ["question", "header", "options"]
                }
            }
        },
        "required": ["questions"]
    })
}

/// Reject malformed input: no questions, a question with no options, an
/// empty option label, or duplicate labels within one question.
pub fn validate(input: &AskInput) -> Result<(), String> {
    if input.questions.is_empty() {
        return Err("AskUserQuestion input must contain at least one question".to_string());
    }

    for q in &input.questions {
        if q.options.is_empty() {
            return Err(format!("question '{}' has no options", q.question));
        }

        let mut seen_labels: Vec<&str> = Vec::new();
        for opt in &q.options {
            if opt.label.trim().is_empty() {
                return Err(format!("question '{}' has an empty option label", q.question));
            }
            if seen_labels.contains(&opt.label.as_str()) {
                return Err(format!(
                    "question '{}' has duplicate option label '{}'",
                    q.question, opt.label
                ));
            }
            seen_labels.push(opt.label.as_str());
        }
    }

    Ok(())
}

/// Render resolved answers as the text handed back to the model.
pub fn format_answers(answers: &[Answer]) -> String {
    answers
        .iter()
        .map(|a| format!("Q: {}\nA: {}", a.question, a.labels.join(", ")))
        .collect::<Vec<String>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn option(label: &str, description: &str) -> QuestionOption {
        QuestionOption {
            label: label.to_string(),
            description: description.to_string(),
        }
    }

    #[test]
    fn deserializes_realistic_tool_argument_payload() {
        let json = serde_json::json!({
            "questions": [
                {
                    "question": "Which database should we use?",
                    "header": "Database",
                    "options": [
                        {"label": "Postgres", "description": "Relational, mature tooling."},
                        {"label": "SQLite", "description": "Zero config, file based."}
                    ],
                    "multiSelect": false
                }
            ]
        });

        let input: AskInput = serde_json::from_value(json).unwrap();
        assert_eq!(input.questions.len(), 1);
        let q = &input.questions[0];
        assert_eq!(q.question, "Which database should we use?");
        assert_eq!(q.header, "Database");
        assert_eq!(q.options.len(), 2);
        assert_eq!(q.options[0].label, "Postgres");
        assert!(!q.multi_select);
    }

    #[test]
    fn multi_select_defaults_to_false_when_absent() {
        let json = serde_json::json!({
            "questions": [
                {
                    "question": "Pick one",
                    "header": "H",
                    "options": [{"label": "A", "description": "d"}]
                }
            ]
        });

        let input: AskInput = serde_json::from_value(json).unwrap();
        assert!(!input.questions[0].multi_select);
    }

    #[test]
    fn validate_rejects_no_questions() {
        let input = AskInput { questions: vec![] };
        let err = validate(&input).unwrap_err();
        assert!(err.contains("at least one question"));
    }

    #[test]
    fn validate_rejects_question_with_no_options() {
        let input = AskInput {
            questions: vec![Question {
                question: "Empty options question".to_string(),
                header: "H".to_string(),
                options: vec![],
                multi_select: false,
            }],
        };
        let err = validate(&input).unwrap_err();
        assert!(err.contains("Empty options question"));
        assert!(err.contains("no options"));
    }

    #[test]
    fn validate_rejects_empty_option_label() {
        let input = AskInput {
            questions: vec![Question {
                question: "Blank label question".to_string(),
                header: "H".to_string(),
                options: vec![option("", "some description")],
                multi_select: false,
            }],
        };
        let err = validate(&input).unwrap_err();
        assert!(err.contains("Blank label question"));
        assert!(err.contains("empty option label"));
    }

    #[test]
    fn validate_rejects_duplicate_labels() {
        let input = AskInput {
            questions: vec![Question {
                question: "Duplicate label question".to_string(),
                header: "H".to_string(),
                options: vec![option("A", "first"), option("A", "second")],
                multi_select: false,
            }],
        };
        let err = validate(&input).unwrap_err();
        assert!(err.contains("Duplicate label question"));
        assert!(err.contains("duplicate option label"));
    }

    #[test]
    fn validate_accepts_well_formed_input() {
        let input = AskInput {
            questions: vec![Question {
                question: "Good question".to_string(),
                header: "H".to_string(),
                options: vec![option("A", "first"), option("B", "second")],
                multi_select: true,
            }],
        };
        assert!(validate(&input).is_ok());
    }

    #[test]
    fn format_answers_renders_single_label() {
        let answers = vec![Answer {
            question: "Which database?".to_string(),
            labels: vec!["Postgres".to_string()],
        }];
        let text = format_answers(&answers);
        assert!(text.contains("Which database?"));
        assert!(text.contains("Postgres"));
    }

    #[test]
    fn format_answers_renders_multi_label() {
        let answers = vec![Answer {
            question: "Which features?".to_string(),
            labels: vec!["Fast".to_string(), "Cheap".to_string()],
        }];
        let text = format_answers(&answers);
        assert!(text.contains("Which features?"));
        assert!(text.contains("Fast, Cheap"));
    }

    #[test]
    fn input_schema_questions_property_is_array_type() {
        let schema = input_schema();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["questions"]["type"], "array");
    }
}
