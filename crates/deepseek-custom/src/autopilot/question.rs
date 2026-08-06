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
