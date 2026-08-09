//! Unit tests for `deepseek_custom::tools::ask` (`src/tools/ask.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use std::sync::Arc;

use async_trait::async_trait;

use deepseek_custom::autopilot::answerer::QuestionAnswerer;
use deepseek_custom::autopilot::question::{self, Answer, AskInput};
use deepseek_custom::error::{HarnessError, Result};
use deepseek_custom::tools::Tool;
use deepseek_custom::tools::ask::AskUserQuestionTool;

struct StubAnswerer {
    result: std::sync::Mutex<Option<Result<Vec<Answer>>>>,
}

impl StubAnswerer {
    fn ok(answers: Vec<Answer>) -> Self {
        Self {
            result: std::sync::Mutex::new(Some(Ok(answers))),
        }
    }

    fn err(msg: &str) -> Self {
        Self {
            result: std::sync::Mutex::new(Some(Err(HarnessError::Tool(msg.to_string())))),
        }
    }
}

#[async_trait]
impl QuestionAnswerer for StubAnswerer {
    async fn answer(&self, _input: &AskInput) -> Result<Vec<Answer>> {
        self.result
            .lock()
            .unwrap()
            .take()
            .expect("answer called more than once in test")
    }
}

fn well_formed_input() -> serde_json::Value {
    serde_json::json!({
        "questions": [
            {
                "question": "Which database should we use?",
                "header": "Database",
                "options": [
                    {"label": "Postgres", "description": "Relational, mature tooling."},
                    {"label": "SQLite", "description": "Zero config, file based."}
                ]
            }
        ]
    })
}

#[tokio::test]
async fn well_formed_question_returns_stub_answer_in_output() {
    let answerer = Arc::new(StubAnswerer::ok(vec![Answer {
        question: "Which database should we use?".to_string(),
        labels: vec!["Postgres".to_string()],
    }]));
    let tool = AskUserQuestionTool::new(answerer);

    let output = tool.execute(well_formed_input()).await.expect("execute");
    assert!(!output.is_error);
    assert!(output.content.contains("Which database should we use?"));
    assert!(output.content.contains("Postgres"));
}

#[tokio::test]
async fn malformed_json_produces_error_tool_output() {
    let answerer = Arc::new(StubAnswerer::ok(vec![]));
    let tool = AskUserQuestionTool::new(answerer);

    let input = serde_json::json!({"not_questions": true});
    let output = tool.execute(input).await.expect("execute returns Ok");
    assert!(output.is_error);
    assert!(output.content.contains("Invalid AskUserQuestion input"));
}

#[tokio::test]
async fn input_failing_validate_produces_error_tool_output() {
    let answerer = Arc::new(StubAnswerer::ok(vec![]));
    let tool = AskUserQuestionTool::new(answerer);

    let input = serde_json::json!({
        "questions": [
            {
                "question": "No options here",
                "header": "H",
                "options": []
            }
        ]
    });
    let output = tool.execute(input).await.expect("execute returns Ok");
    assert!(output.is_error);
    assert!(output.content.contains("no options"));
}

#[tokio::test]
async fn answerer_error_produces_error_tool_output() {
    let answerer = Arc::new(StubAnswerer::err("policy call failed"));
    let tool = AskUserQuestionTool::new(answerer);

    let output = tool
        .execute(well_formed_input())
        .await
        .expect("execute returns Ok");
    assert!(output.is_error);
    assert!(output.content.contains("policy call failed"));
}

#[test]
fn input_schema_matches_autopilot_question_schema() {
    let answerer = Arc::new(StubAnswerer::ok(vec![]));
    let tool = AskUserQuestionTool::new(answerer);
    assert_eq!(tool.input_schema(), question::input_schema());
}
