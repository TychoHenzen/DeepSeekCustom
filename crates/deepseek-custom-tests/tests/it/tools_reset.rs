//! Unit tests for `deepseek_custom::tools::reset` (`src/tools/reset.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use deepseek_custom::error::HarnessError;
use deepseek_custom::tools::Tool;
use deepseek_custom::tools::reset::ResetTool;

#[tokio::test]
async fn returns_session_reset_error() {
    let tool = ResetTool;
    let input = serde_json::json!({"prompt": "start fresh"});
    let result = tool.execute(input).await;

    assert!(result.is_err());
    match result.unwrap_err() {
        HarnessError::SessionReset => {} // expected
        other => panic!("expected SessionReset, got {other:?}"),
    }
}
