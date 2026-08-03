use std::sync::Arc;

use async_trait::async_trait;

use crate::autopilot::answerer::QuestionAnswerer;
use crate::autopilot::question::{self, AskInput};
use crate::error::Result;
use crate::tools::{Tool, ToolOutput};

/// Tool that asks the user a question during a normal run, or an autopilot
/// policy during an unattended run. Never blocks: the answerer always
/// resolves to something, even on a model or network failure.
pub struct AskUserQuestionTool {
    answerer: Arc<dyn QuestionAnswerer>,
}

impl AskUserQuestionTool {
    pub fn new(answerer: Arc<dyn QuestionAnswerer>) -> Self {
        Self { answerer }
    }
}

#[async_trait]
impl Tool for AskUserQuestionTool {
    fn name(&self) -> &str {
        "AskUserQuestion"
    }

    fn description(&self) -> &str {
        "Ask the user a question with a small set of labelled options and get back the chosen \
        answer. Answers come from a policy the user wrote ahead of time, so this tool never \
        blocks. Use it for genuine decisions the user needs to make, not for questions you can \
        answer yourself by reading the code."
    }

    fn input_schema(&self) -> serde_json::Value {
        question::input_schema()
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput> {
        let parsed: AskInput = match serde_json::from_value(input) {
            Ok(parsed) => parsed,
            Err(e) => {
                return Ok(ToolOutput {
                    content: format!("Invalid AskUserQuestion input: {e}"),
                    is_error: true,
                });
            }
        };

        if let Err(e) = question::validate(&parsed) {
            return Ok(ToolOutput {
                content: format!("Invalid AskUserQuestion input: {e}"),
                is_error: true,
            });
        }

        match self.answerer.answer(&parsed).await {
            Ok(answers) => Ok(ToolOutput {
                content: question::format_answers(&answers),
                is_error: false,
            }),
            Err(e) => Ok(ToolOutput {
                content: format!("Failed to answer question: {e}"),
                is_error: true,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autopilot::question::Answer;
    use crate::error::HarnessError;

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

        let output = tool.execute(well_formed_input()).await.expect("execute returns Ok");
        assert!(output.is_error);
        assert!(output.content.contains("policy call failed"));
    }

    #[test]
    fn input_schema_matches_autopilot_question_schema() {
        let answerer = Arc::new(StubAnswerer::ok(vec![]));
        let tool = AskUserQuestionTool::new(answerer);
        assert_eq!(tool.input_schema(), question::input_schema());
    }
}
