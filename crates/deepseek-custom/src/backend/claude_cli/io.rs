//! Spawn background tasks that read the child's stdout and stderr for the
//! lifetime of one `claude -p` process.

use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

use crate::agent::events::{RoutedEvent, StreamEvent};

use super::events::{ClaudeEvent, parse_line};
use super::map::EventMapper;

/// Read the child's stdout forever, publishing mapped events. Signals
/// `turn_done` after the terminal `result` event of each turn, so `send`
/// knows when one turn ended. Sends the child's session id on
/// `session_id` once `EventMapper` reads it from the `init` event. The
/// driver then picks it up and reuses it as `--resume`. Dropping the
/// senders when the loop ends also releases a `send` waiting on a child
/// that died mid-turn.
pub(super) fn spawn_stdout_reader(
    stdout: tokio::process::ChildStdout,
    tx_events: mpsc::UnboundedSender<RoutedEvent>,
    turn_done: mpsc::UnboundedSender<()>,
    session_id: mpsc::UnboundedSender<String>,
    last_reply: Arc<Mutex<String>>,
) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        let mut mapper = EventMapper::new();
        let mut session_id_sent = false;
        loop {
            let next = match lines.next_line().await {
                Ok(Some(line)) => line,
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!("claude_cli: error reading stdout: {e}");
                    let _ = tx_events.send(RoutedEvent::own(StreamEvent::Error {
                        message: format!("claude CLI stdout read error: {e}"),
                    }));
                    break;
                }
            };
            let Some(event) = parse_line(&next) else {
                continue;
            };
            let ends_turn = matches!(event, ClaudeEvent::Result(_));
            for stream_event in mapper.map(event) {
                if let StreamEvent::Text { text, .. } = &stream_event
                    && let Ok(mut reply) = last_reply.lock()
                {
                    reply.push_str(text.as_str());
                }
                if tx_events.send(RoutedEvent::own(stream_event)).is_err() {
                    return;
                }
            }
            if !session_id_sent && let Some(id) = mapper.session_id() {
                session_id_sent = true;
                if session_id.send(id.to_string()).is_err() {
                    return;
                }
            }
            if ends_turn && turn_done.send(()).is_err() {
                return;
            }
        }
    });
}

pub(super) fn spawn_stderr_drain(stderr: tokio::process::ChildStderr) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    if !line.trim().is_empty() {
                        tracing::warn!("claude_cli stderr: {line}");
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!("claude_cli: error reading stderr: {e}");
                    break;
                }
            }
        }
    });
}
