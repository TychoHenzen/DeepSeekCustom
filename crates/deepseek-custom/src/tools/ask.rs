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
                    image: None,
                });
            }
        };

        if let Err(e) = question::validate(&parsed) {
            return Ok(ToolOutput {
                content: format!("Invalid AskUserQuestion input: {e}"),
                is_error: true,
                image: None,
            });
        }

        match self.answerer.answer(&parsed).await {
            Ok(answers) => Ok(ToolOutput {
                content: question::format_answers(&answers),
                is_error: false,
                image: None,
            }),
            Err(e) => Ok(ToolOutput {
                content: format!("Failed to answer question: {e}"),
                is_error: true,
                image: None,
            }),
        }
    }
}

