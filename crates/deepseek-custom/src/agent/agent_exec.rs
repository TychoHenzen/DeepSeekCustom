use std::ops::ControlFlow;
use std::sync::atomic::Ordering;

use tracing::{info, warn};

use crate::api::types::{Content, Message, Role, ToolCall, Usage};
use crate::error::HarnessError;
use crate::tools::ToolOutput;

use super::agent_helpers::build_user_content;
use super::agent_loop::AgentLoop;
use super::events::StreamEvent;

impl AgentLoop {
    /// Execute one batch of tool calls and handle any tool-returned images.
    pub(crate) async fn run_tool_calls(
        &mut self,
        turn: u32,
        tool_calls: &[ToolCall],
        finish_reason: &str,
        stream_usage: &Option<Usage>,
    ) -> ControlFlow<()> {
        for tc in tool_calls {
            if self.interrupt_flag.load(Ordering::SeqCst) {
                info!("agent: user interrupted before tool execution");
                self.interrupt_flag.store(false, Ordering::SeqCst);
                self.send_event(StreamEvent::Interrupted {
                    message: "Interrupted by user (Escape)".into(),
                });
                return ControlFlow::Break(());
            }

            let func = tc.function.as_ref();
            let tool_name = func.and_then(|f| f.name.as_deref()).unwrap_or("unknown");
            let tool_args = func.and_then(|f| f.arguments.as_deref()).unwrap_or("{}");

            self.send_event(StreamEvent::ToolCallStart {
                turn: turn + 1,
                tool: tool_name.to_string(),
                args: tool_args.to_string(),
            });

            let result = self.execute_tool(tool_name, tool_args).await;

            self.send_event(StreamEvent::ToolCallEnd {
                turn: turn + 1,
                tool: tool_name.to_string(),
                output: result.content.clone(),
                is_error: result.is_error,
            });

            self.history.push(Message {
                role: Role::Tool,
                content: Some(Content::text(result.content)),
                tool_calls: None,
                tool_call_id: Some(tc.id.clone()),
                reasoning_content: None,
            });

            if let Some(image) = result.image {
                let built = build_user_content(
                    self.client.provider(),
                    "(image read by the tool call above)",
                    Some(&image),
                );
                if let Some(message) = built.notice {
                    self.send_event(StreamEvent::Error { message });
                }
                self.push_user_message(built.content);
            }
        }

        self.send_snapshot_and_turn_end(turn, finish_reason, stream_usage);
        ControlFlow::Continue(())
    }

    pub(crate) async fn execute_tool(&self, name: &str, args: &str) -> ToolOutput {
        let input: serde_json::Value = match serde_json::from_str(args) {
            Ok(v) => v,
            Err(e) => {
                warn!("execute_tool: failed to parse args for '{}': {}", name, e);
                return ToolOutput {
                    content: self.arg_parse_error(name, args, &e),
                    is_error: true,
                    image: None,
                };
            }
        };

        match self.tools.get(name) {
            Some(tool) => match tool.execute(input).await {
                Ok(output) => output,
                Err(HarnessError::SessionReset) => {
                    info!("session reset triggered by tool '{}'", name);
                    if let Some(registry) = self.subagent_registry.clone() {
                        registry.close_all().await;
                    }
                    self.send_event(StreamEvent::SessionReset);
                    ToolOutput {
                        content: "Session reset initiated.".into(),
                        is_error: false,
                        image: None,
                    }
                }
                Err(e) => ToolOutput {
                    content: format!("Tool error: {e}"),
                    is_error: true,
                    image: None,
                },
            },
            None => ToolOutput {
                content: format!(
                    "Unknown tool: {name}. Available tools: {}",
                    self.tool_name_list()
                ),
                is_error: true,
                image: None,
            },
        }
    }

    fn arg_parse_error(&self, name: &str, args: &str, e: &serde_json::Error) -> String {
        if e.classify() != serde_json::error::Category::Eof {
            return format!("Tool error: Invalid input: {e}");
        }
        format!(
            "Tool error: the arguments for '{name}' stop part way \
             through ({e}). The reply ran into the {} token output \
             cap while writing them, so the call never finished. It \
             carried {} characters. Retry with a smaller payload: \
             use `edit` to change part of a file rather than \
             `write` to replace all of it, or write the file in \
             several smaller calls. Raise `max_tokens` in \
             settings.json if the payload cannot be split.",
            self.config.max_tokens,
            args.len(),
        )
    }

    fn tool_name_list(&self) -> String {
        let mut names: Vec<String> = self
            .tools
            .list()
            .iter()
            .map(|tool| tool.name().to_string())
            .collect();
        names.sort();
        names.join(", ")
    }

    #[cfg(feature = "test-support")]
    pub async fn execute_tool_for_test(&self, name: &str, args: &str) -> ToolOutput {
        self.execute_tool(name, args).await
    }
}
