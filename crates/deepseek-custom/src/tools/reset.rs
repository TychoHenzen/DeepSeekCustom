use async_trait::async_trait;
use serde::Deserialize;
use tracing::info;

use crate::error::{HarnessError, Result};
use crate::tools::{Tool, ToolOutput};

/// Session reset tool. Returns `HarnessError::SessionReset` so the agent loop
/// can clear history and reload memory files.
pub struct ResetTool;

#[derive(Deserialize)]
struct ResetInput {
    prompt: String,
}

#[async_trait]
impl Tool for ResetTool {
    fn name(&self) -> &str {
        "reset"
    }

    fn description(&self) -> &str {
        "Reset the session: clear conversation history, reload memory files, start fresh with the given prompt."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The initial prompt for the new session"
                }
            },
            "required": ["prompt"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput> {
        let _parsed: ResetInput = serde_json::from_value(input)
            .map_err(|e| HarnessError::Tool(format!("Invalid reset input: {e}")))?;

        info!("reset: session reset triggered");
        // The agent loop catches this variant to perform the actual reset.
        Err(HarnessError::SessionReset)
    }
}

