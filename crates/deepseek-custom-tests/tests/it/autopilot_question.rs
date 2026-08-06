//! Unit tests for `deepseek_custom::autopilot::question`, moved out of the
//! production module as part of the two-crate workspace split.

use deepseek_custom::autopilot::question::{
    format_answers, input_schema, validate, Answer, AskInput, Question, QuestionOption,
};

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
