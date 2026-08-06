//! Unit tests for `deepseek_custom::autopilot::answerer`, moved out of the
//! production module as part of the two-crate workspace split.

use deepseek_custom::autopilot::answerer::{build_prompt, parse_reply, resolve_answers};
use deepseek_custom::autopilot::question::{Answer, AskInput, Question, QuestionOption};

fn option(label: &str, description: &str) -> QuestionOption {
    QuestionOption {
        label: label.to_string(),
        description: description.to_string(),
    }
}

fn simple_question(question: &str, header: &str, labels: &[&str], multi_select: bool) -> Question {
    Question {
        question: question.to_string(),
        header: header.to_string(),
        options: labels
            .iter()
            .map(|l| option(l, &format!("{l} description")))
            .collect(),
        multi_select,
    }
}

#[test]
fn prompt_contains_policy_questions_and_options() {
    let input = AskInput {
        questions: vec![simple_question(
            "Which database?",
            "Database",
            &["Postgres", "SQLite"],
            false,
        )],
    };
    let prompt = build_prompt("## Autopilot Policy\n\nBe concise.", &input);
    assert!(prompt.contains("Be concise."));
    assert!(prompt.contains("Which database?"));
    assert!(prompt.contains("Postgres"));
    assert!(prompt.contains("SQLite"));
}

#[test]
fn parse_reply_handles_bare_json_array() {
    let reply = r#"[{"question":"Q1","labels":["A"]}]"#;
    let answers = parse_reply(reply).expect("parses");
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].question, "Q1");
    assert_eq!(answers[0].labels, vec!["A".to_string()]);
}

#[test]
fn parse_reply_handles_fenced_code_block() {
    let reply = "```json\n[{\"question\":\"Q1\",\"labels\":[\"A\"]}]\n```";
    let answers = parse_reply(reply).expect("parses");
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].question, "Q1");
}

#[test]
fn parse_reply_returns_none_on_garbage() {
    assert!(parse_reply("not json at all").is_none());
}

#[test]
fn resolve_answers_falls_back_to_first_option_when_parse_returned_none() {
    let input = AskInput {
        questions: vec![simple_question("Q1", "H", &["A", "B"], false)],
    };
    let answers = resolve_answers(None, &input);
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].labels, vec!["A".to_string()]);
}

#[test]
fn resolve_answers_replaces_answer_with_label_not_in_options() {
    let input = AskInput {
        questions: vec![simple_question("Q1", "H", &["A", "B"], false)],
    };
    let parsed = vec![Answer {
        question: "Q1".to_string(),
        labels: vec!["Z".to_string()],
    }];
    let answers = resolve_answers(Some(parsed), &input);
    assert_eq!(answers[0].labels, vec!["A".to_string()]);
}

#[test]
fn resolve_answers_trims_multi_label_to_one_on_single_select() {
    let input = AskInput {
        questions: vec![simple_question("Q1", "H", &["A", "B"], false)],
    };
    let parsed = vec![Answer {
        question: "Q1".to_string(),
        labels: vec!["A".to_string(), "B".to_string()],
    }];
    let answers = resolve_answers(Some(parsed), &input);
    assert_eq!(answers[0].labels, vec!["A".to_string()]);
}

#[test]
fn resolve_answers_keeps_valid_multi_label_answer_on_multi_select() {
    let input = AskInput {
        questions: vec![simple_question("Q1", "H", &["A", "B"], true)],
    };
    let parsed = vec![Answer {
        question: "Q1".to_string(),
        labels: vec!["A".to_string(), "B".to_string()],
    }];
    let answers = resolve_answers(Some(parsed), &input);
    assert_eq!(answers[0].labels, vec!["A".to_string(), "B".to_string()]);
}

#[test]
fn resolve_answers_keeps_input_question_order() {
    let input = AskInput {
        questions: vec![
            simple_question("Q1", "H1", &["A"], false),
            simple_question("Q2", "H2", &["B"], false),
        ],
    };
    let parsed = vec![
        Answer {
            question: "Q2".to_string(),
            labels: vec!["B".to_string()],
        },
        Answer {
            question: "Q1".to_string(),
            labels: vec!["A".to_string()],
        },
    ];
    let answers = resolve_answers(Some(parsed), &input);
    assert_eq!(answers[0].question, "Q1");
    assert_eq!(answers[1].question, "Q2");
}
