use std::sync::atomic::Ordering;

use tokio::sync::mpsc;
use tracing::{debug, error};

use crate::api::types::{StreamChunk, ToolCall};
use crate::error::HarnessError;

use super::agent_helpers::{StreamCollection, merge_tool_call};
use super::agent_loop::AgentLoop;
use super::events::StreamEvent;

impl AgentLoop {
    /// Collect a stream of SSE chunks into a `StreamCollection`.
    pub(crate) async fn collect_stream(
        &mut self,
        rx: &mut mpsc::UnboundedReceiver<std::result::Result<StreamChunk, HarnessError>>,
    ) -> StreamCollection {
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut finish_reason = String::new();
        let mut usage: Option<crate::api::types::Usage> = None;
        let mut interrupted = false;

        while let Some(chunk_result) = rx.recv().await {
            if self.interrupt_flag.load(Ordering::SeqCst) {
                debug!("interrupt detected during stream receive");
                interrupted = true;
                break;
            }
            match chunk_result {
                Ok(chunk) => {
                    self.apply_chunk(
                        &chunk,
                        &mut text,
                        &mut reasoning,
                        &mut tool_calls,
                        &mut finish_reason,
                        &mut usage,
                    );
                }
                Err(e) => {
                    error!("stream error: {e}");
                    self.send_event(StreamEvent::Error {
                        message: format!("{e}"),
                    });
                    interrupted = true;
                    break;
                }
            }
        }

        debug!(
            "stream complete: text_len={}, reasoning_len={}, \
             tool_calls={}, finish={}",
            text.len(),
            reasoning.len(),
            tool_calls.len(),
            finish_reason,
        );

        StreamCollection {
            text,
            reasoning,
            tool_calls,
            finish_reason,
            usage,
            interrupted,
        }
    }

    fn apply_chunk(
        &self,
        chunk: &StreamChunk,
        text: &mut String,
        reasoning: &mut String,
        tool_calls: &mut Vec<ToolCall>,
        finish_reason: &mut String,
        usage: &mut Option<crate::api::types::Usage>,
    ) {
        self.process_chunk(chunk, text, reasoning, tool_calls, finish_reason);
        if chunk.usage.is_some() {
            *usage = chunk.usage.clone();
        }
    }

    /// Process one SSE chunk into the accumulators.
    pub(crate) fn process_chunk(
        &self,
        chunk: &StreamChunk,
        text: &mut String,
        reasoning: &mut String,
        tool_calls: &mut Vec<ToolCall>,
        finish_reason: &mut String,
    ) {
        if let Some(ref choices) = chunk.choices {
            for choice in choices {
                if let Some(ref content) = choice.delta.content {
                    text.push_str(content);
                    self.send_event(StreamEvent::Text {
                        turn: 1,
                        text: content.clone(),
                    });
                }
                if let Some(ref r) = choice.delta.reasoning_content {
                    reasoning.push_str(r);
                    self.send_event(StreamEvent::Reasoning {
                        turn: 1,
                        text: r.clone(),
                    });
                }
                if let Some(ref tcs) = choice.delta.tool_calls {
                    for tc in tcs {
                        merge_tool_call(tool_calls, tc);
                    }
                }
                if let Some(ref fr) = choice.finish_reason {
                    finish_reason.clone_from(fr);
                }
            }
        }
    }
}
