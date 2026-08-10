use std::sync::atomic::Ordering;

use tokio::sync::mpsc;
use tracing::{debug, error};

use crate::api::types::{StreamChoice, StreamChunk, ToolCall};
use crate::error::HarnessError;

use super::agent_helpers::{StreamCollection, merge_tool_call};
use super::agent_loop::AgentLoop;
use super::events::StreamEvent;

/// Mutable refs to the accumulators built up during stream collection.
struct ChunkAccum<'a> {
    text: &'a mut String,
    reasoning: &'a mut String,
    tool_calls: &'a mut Vec<ToolCall>,
    finish_reason: &'a mut String,
    usage: &'a mut Option<crate::api::types::Usage>,
}

impl AgentLoop {
    /// Collect a stream of SSE chunks into a `StreamCollection`.
    pub(crate) async fn collect_stream(
        &mut self,
        rx: &mut mpsc::UnboundedReceiver<Result<StreamChunk, HarnessError>>,
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
            let mut accum = ChunkAccum {
                text: &mut text,
                reasoning: &mut reasoning,
                tool_calls: &mut tool_calls,
                finish_reason: &mut finish_reason,
                usage: &mut usage,
            };
            if self.handle_stream_result(chunk_result, &mut accum) {
                interrupted = true;
                break;
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

    /// Process one result from the stream. Returns true to stop collecting.
    fn handle_stream_result(
        &mut self,
        result: Result<StreamChunk, HarnessError>,
        accum: &mut ChunkAccum<'_>,
    ) -> bool {
        match result {
            Ok(chunk) => {
                self.process_chunk(&chunk, accum);
                false
            }
            Err(e) => {
                error!("stream error: {e}");
                self.send_event(StreamEvent::Error {
                    message: format!("{e}"),
                });
                true
            }
        }
    }

    fn process_chunk(&self, chunk: &StreamChunk, accum: &mut ChunkAccum<'_>) {
        if let Some(ref choices) = chunk.choices {
            for choice in choices {
                process_choice_delta(self, choice, accum);
            }
        }
        if chunk.usage.is_some() {
            *accum.usage = chunk.usage.clone();
        }
    }
}

/// Process the delta fields of one `StreamChoice` into the accumulators.
fn process_choice_delta(agent: &AgentLoop, choice: &StreamChoice, accum: &mut ChunkAccum<'_>) {
    if let Some(ref content) = choice.delta.content {
        accum.text.push_str(content);
        agent.send_event(StreamEvent::Text {
            turn: 1,
            text: content.clone(),
        });
    }
    if let Some(ref r) = choice.delta.reasoning_content {
        accum.reasoning.push_str(r);
        agent.send_event(StreamEvent::Reasoning {
            turn: 1,
            text: r.clone(),
        });
    }
    if let Some(ref tcs) = choice.delta.tool_calls {
        for tc in tcs {
            merge_tool_call(accum.tool_calls, tc);
        }
    }
    if let Some(ref fr) = choice.finish_reason {
        accum.finish_reason.clone_from(fr);
    }
}
