use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::api::client::DeepSeekClient;
use crate::api::types::{ChatRequest, Message, Role, ToolCall};
use crate::error::{HarnessError, Result};
use crate::tools::{ToolOutput, ToolRegistry};

use super::history::MessageHistory;
use super::prompt::SystemPromptBuilder;

/// Configuration for the agent loop.
pub struct AgentConfig {
    pub max_turns: u32,
    pub model: String,
    pub thinking: bool,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_turns: 100,
            model: "deepseek-v4-flash".into(),
            thinking: false,
        }
    }
}

/// Events sent from the agent loop to the TUI (or caller) during streaming.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// Text chunk received from the model.
    Text { turn: u32, text: String },
    /// A tool call has started.
    ToolCallStart { turn: u32, tool: String, args: String },
    /// A tool call completed.
    ToolCallEnd { turn: u32, tool: String, output: String, is_error: bool },
    /// The agent has finished its turn.
    TurnEnd { turn: u32, finish_reason: String },
    /// Session was reset.
    SessionReset,
    /// An error occurred.
    Error { message: String },
}

/// Core agent loop: user input → API call → tool execution → repeat.
pub struct AgentLoop {
    client: DeepSeekClient,
    tools: ToolRegistry,
    history: MessageHistory,
    config: AgentConfig,
    tx_events: Option<mpsc::UnboundedSender<StreamEvent>>,
}

impl AgentLoop {
    /// Create a new AgentLoop.
    pub fn new(
        client: DeepSeekClient,
        tools: ToolRegistry,
        system_prompt: String,
        config: AgentConfig,
    ) -> Self {
        Self {
            client,
            tools,
            history: MessageHistory::new(system_prompt),
            config,
            tx_events: None,
        }
    }

    /// Set the event sender for streaming output.
    pub fn set_event_sender(&mut self, tx: mpsc::UnboundedSender<StreamEvent>) {
        self.tx_events = Some(tx);
    }

    /// Run the agent loop for a single user message.
    /// Returns the final assistant text or an error.
    pub async fn run(&mut self, user_input: &str) -> Result<Vec<String>> {
        // Add user message
        self.history.push(Message {
            role: Role::User,
            content: Some(user_input.to_string()),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        });

        let mut assistant_texts: Vec<String> = Vec::new();

        for turn in 0..self.config.max_turns {
            debug!("turn {}/{}", turn + 1, self.config.max_turns);

            // Build API request
            let tools = self.tools.to_api_definitions();
            let messages = self.history.to_api_messages();

            let request = ChatRequest {
                model: self.config.model.clone(),
                messages,
                tools: if tools.is_empty() { None } else { Some(tools) },
                tool_choice: None,
                stream: true,
                temperature: Some(0.7),
                max_tokens: Some(4096),
                thinking: if self.config.thinking {
                    Some(crate::api::types::ThinkingConfig {
                        thinking_type: "enabled".into(),
                        reasoning_effort: None,
                    })
                } else {
                    None
                },
            };

            // Call API (streaming)
            let mut rx = self.client.chat_stream(&request);

            let mut stream_text = String::new();
            let mut stream_tool_calls: Vec<ToolCall> = Vec::new();
            let mut finish_reason = String::new();

            while let Some(chunk_result) = rx.recv().await {
                match chunk_result {
                    Ok(chunk) => {
                        if let Some(ref choices) = chunk.choices {
                            for choice in choices {
                                // Accumulate text
                                if let Some(ref content) = choice.delta.content {
                                    stream_text.push_str(content);
                                    self.send_event(StreamEvent::Text {
                                        turn: turn + 1,
                                        text: content.clone(),
                                    });
                                }
                                // Accumulate tool calls
                                if let Some(ref tcs) = choice.delta.tool_calls {
                                    for tc in tcs {
                                        stream_tool_calls.push(tc.clone());
                                    }
                                }
                                // Track finish reason
                                if let Some(ref fr) = choice.finish_reason {
                                    finish_reason = fr.clone();
                                }
                            }
                        }
                    }
                    Err(e) => {
                        self.send_event(StreamEvent::Error {
                            message: format!("{e}"),
                        });
                        return Err(e);
                    }
                }
            }

            self.send_event(StreamEvent::TurnEnd {
                turn: turn + 1,
                finish_reason: finish_reason.clone(),
            });

            // Handle tool calls or text response
            if !stream_tool_calls.is_empty() {
                // Append assistant message with tool calls
                self.history.push(Message {
                    role: Role::Assistant,
                    content: if stream_text.is_empty() { None } else { Some(stream_text.clone()) },
                    tool_calls: Some(stream_tool_calls.clone()),
                    tool_call_id: None,
                    reasoning_content: None,
                });

                // Execute each tool call
                for tc in &stream_tool_calls {
                    let tool_name = &tc.function.name;
                    let tool_args = &tc.function.arguments;

                    self.send_event(StreamEvent::ToolCallStart {
                        turn: turn + 1,
                        tool: tool_name.clone(),
                        args: tool_args.clone(),
                    });

                    let result = self.execute_tool(tool_name, tool_args).await;

                    self.send_event(StreamEvent::ToolCallEnd {
                        turn: turn + 1,
                        tool: tool_name.clone(),
                        output: result.content.clone(),
                        is_error: result.is_error,
                    });

                    // Feed tool result back to history
                    self.history.push(Message {
                        role: Role::Tool,
                        content: Some(result.content),
                        tool_calls: None,
                        tool_call_id: Some(tc.id.clone()),
                        reasoning_content: None,
                    });
                }
            } else {
                // Text-only response — done
                if !stream_text.is_empty() {
                    assistant_texts.push(stream_text.clone());
                }
                self.history.push(Message {
                    role: Role::Assistant,
                    content: Some(stream_text.clone()),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                });
                break;
            }
        }

        // Max turns check
        if assistant_texts.is_empty() {
            warn!("max turns reached without final text response");
        }

        Ok(assistant_texts)
    }

    /// Execute a tool by name, handling SessionReset specially.
    async fn execute_tool(&self, name: &str, args: &str) -> ToolOutput {
        let input: serde_json::Value = serde_json::from_str(args).unwrap_or(serde_json::Value::Null);

        match self.tools.get(name) {
            Some(tool) => match tool.execute(input).await {
                Ok(output) => output,
                Err(HarnessError::SessionReset) => {
                    info!("session reset triggered by tool '{}'", name);
                    self.send_event(StreamEvent::SessionReset);
                    ToolOutput {
                        content: "Session reset initiated.".into(),
                        is_error: false,
                    }
                }
                Err(e) => ToolOutput {
                    content: format!("Tool error: {e}"),
                    is_error: true,
                },
            },
            None => ToolOutput {
                content: format!("Unknown tool: {name}"),
                is_error: true,
            },
        }
    }

    /// Send a StreamEvent to the TUI if a sender is configured.
    fn send_event(&self, event: StreamEvent) {
        if let Some(ref tx) = self.tx_events {
            let _ = tx.send(event);
        }
    }

    /// Access the message history (for display).
    pub fn history(&self) -> &MessageHistory {
        &self.history
    }

    /// Clear history and reload system prompt (called after session reset).
    pub fn reset(&mut self, new_system_prompt: String, new_user_prompt: String) {
        info!("agent: session reset");
        self.history = MessageHistory::new(new_system_prompt);
        self.history.push(Message {
            role: Role::User,
            content: Some(new_user_prompt),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        });
        self.send_event(StreamEvent::SessionReset);
    }

    /// Rebuild system prompt from components (used after memory reload).
    pub fn rebuild_system_prompt(
        &mut self,
        memory_fragment: Option<&str>,
        skills_fragment: Option<&str>,
    ) {
        let builder = SystemPromptBuilder::new();
        let tools = self.tools.to_api_definitions();
        let new_prompt = builder.build(memory_fragment, skills_fragment, &tools);
        self.history = MessageHistory::new(new_prompt);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::tools::{Tool, ToolOutput, ToolRegistry};

    struct EchoTool;
    #[async_trait::async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str { "echo" }
        fn description(&self) -> &str { "echoes input" }
        fn input_schema(&self) -> serde_json::Value { serde_json::json!({}) }
        async fn execute(&self, _input: serde_json::Value) -> Result<ToolOutput> {
            Ok(ToolOutput { content: "echoed".into(), is_error: false })
        }
    }

    #[test]
    fn agent_config_defaults() {
        let cfg = AgentConfig::default();
        assert_eq!(cfg.max_turns, 100);
        assert_eq!(cfg.model, "deepseek-v4-flash");
        assert!(!cfg.thinking);
    }

    #[test]
    fn agent_loop_creates_with_history() {
        let client = DeepSeekClient::new("sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let agent = AgentLoop::new(client, tools, "test prompt".into(), AgentConfig::default());
        assert_eq!(agent.history().len(), 0);
    }

    #[test]
    fn session_reset_clears_history() {
        let client = DeepSeekClient::new("sk-test".into(), None, None);
        let mut tools = ToolRegistry::new();
        tools.register(Arc::new(EchoTool));
        let mut agent = AgentLoop::new(client, tools, "initial".into(), AgentConfig::default());

        // Push a user message then reset
        agent.history.push(Message::user("hello".into()));
        assert_eq!(agent.history().len(), 1);

        agent.reset("fresh prompt".into(), "restart".into());
        // After reset: fresh system + 1 user message
        assert_eq!(agent.history().len(), 1);
    }

    #[test]
    fn rebuild_system_prompt_clears_messages() {
        let client = DeepSeekClient::new("sk-test".into(), None, None);
        let mut tools = ToolRegistry::new();
        tools.register(Arc::new(EchoTool));
        let mut agent = AgentLoop::new(client, tools, "initial".into(), AgentConfig::default());
        agent.history.push(Message::user("hi".into()));

        agent.rebuild_system_prompt(Some("memory"), Some("skills"));
        assert_eq!(agent.history().len(), 0);
    }

    #[tokio::test]
    async fn execute_tool_returns_output() {
        let client = DeepSeekClient::new("sk-test".into(), None, None);
        let mut tools = ToolRegistry::new();
        tools.register(Arc::new(EchoTool));
        let agent = AgentLoop::new(client, tools, "test".into(), AgentConfig::default());

        let output = agent.execute_tool("echo", "{}").await;
        assert!(!output.is_error);
        assert_eq!(output.content, "echoed");
    }

    #[tokio::test]
    async fn execute_unknown_tool_returns_error() {
        let client = DeepSeekClient::new("sk-test".into(), None, None);
        let tools = ToolRegistry::new();
        let agent = AgentLoop::new(client, tools, "test".into(), AgentConfig::default());

        let output = agent.execute_tool("nonexistent", "{}").await;
        assert!(output.is_error);
        assert!(output.content.contains("Unknown tool"));
    }
}
