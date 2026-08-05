//! Fake `claude` binary for tests. Replays a minimal stream-json session on
//! stdout and reads stdin, so `ClaudeCliDriver` can be driven end to end in
//! a test without spawning the real `claude` CLI and without any network
//! call or cost.
//!
//! Protocol, closely following the real one confirmed against
//! `tests/fixtures/claude_stream_json.jsonl` and
//! `tests/fixtures/claude_stream_json_tools.jsonl`:
//!
//! - On startup, before reading any stdin, it emits one
//!   `{"type":"system","subtype":"init",...}` line carrying a
//!   `session_id`. That id is reused from a `--resume <id>` argument when
//!   one is given, exactly as a real resumed session keeps its id, and is
//!   otherwise a fresh v4 uuid.
//! - It then reads stdin one line at a time. Each line is one user turn in
//!   the verified shape `{"type":"user","message":{"role":"user",
//!   "content":[{"type":"text","text":"..."}]}}`. For each turn it emits a
//!   small `stream_event` sequence (`content_block_start`,
//!   `content_block_delta` with a `text_delta`, `content_block_stop`) that
//!   echoes the turn's text back prefixed with `echo: `, then a
//!   `{"type":"result",...}` line carrying the same `session_id` and the
//!   echoed text as `result`. `ClaudeCliDriver::send` waits for exactly
//!   this event to know the turn is done.
//! - Every line is flushed as it is written, so a caller reading stdout
//!   with a line reader sees each event as soon as it is emitted, not
//!   batched at exit.
//!
//! What this fake can and cannot do, for the lifecycle tests P7S02 builds
//! on top of this: it supports any number of turns in one run, each ending
//! its own `result` event, so a turn-boundary test can send two turns and
//! check the driver waits for the first result before the second import is
//! not started early. It supports `--resume`, so a respawn-with-resume test
//! can check the second child's `init` event carries the same id the first
//! child reported. It exits cleanly when stdin closes, which is what a
//! `shutdown()` call against a real child produces, so a
//! respawn-after-shutdown test can rely on that. It has no way to be
//! killed and then observe that from inside the process, so the
//! interrupt-kills-the-child behaviour is exercised by asserting on the
//! driver and OS process state after `interrupt()`, not by anything this
//! binary does differently. It never varies its reply by `--effort`,
//! `--append-system-prompt`, or `--model`: those args are accepted (an
//! unknown flag is simply not read) but do not change the output, since no
//! planned P7S02 test needs voice-mode text or effort level reflected back
//! in the reply itself, only that a respawn happened. It never emits
//! thinking deltas, tool_use blocks, or more than one content block in a
//! turn: the fixtures already cover that shape in the parser and mapper
//! tests, and no lifecycle test needs a fake tool call.
//!
//! Two markers, added for P7S02's lifecycle tests, since neither test can
//! be made deterministic against a fake that always replies instantly:
//!
//! - A turn whose text contains `HANG_MARKER` sleeps for
//!   `HANG_SLEEP_SECS` before replying, instead of replying at once. This
//!   gives an interrupt test a wide, reliable window to kill the child
//!   mid-turn instead of racing an instant reply.
//! - A turn whose text contains `EXIT_MARKER` replies normally, then exits
//!   the process instead of reading the next stdin line. This models a
//!   child that ends itself between turns, for a respawn test, without
//!   needing a way to kill the process from outside.
//!
//! Both are plain substring checks on the echoed text, so a lifecycle test
//! opts in by including the marker in the text it sends, and every other
//! test is unaffected.

use std::io::{self, BufRead, Write};

/// See the module doc comment above.
const HANG_MARKER: &str = "__FAKE_CLAUDE_HANG__";
/// See the module doc comment above.
const EXIT_MARKER: &str = "__FAKE_CLAUDE_EXIT_AFTER_REPLY__";
/// How long a turn carrying `HANG_MARKER` sleeps before replying. Long
/// enough that a test's interrupt always lands well before it, short
/// enough that a broken interrupt still fails the test in bounded time
/// instead of hanging the suite.
const HANG_SLEEP_SECS: u64 = 3;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let resume_id = find_flag_value(&args, "--resume");
    let session_id = resume_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let stdout = io::stdout();
    let mut out = stdout.lock();
    emit_init(&mut out, &session_id);

    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let text = extract_user_text(&line).unwrap_or_default();
        if text.contains(HANG_MARKER) {
            std::thread::sleep(std::time::Duration::from_secs(HANG_SLEEP_SECS));
        }
        emit_turn(&mut out, &session_id, &text);
        if text.contains(EXIT_MARKER) {
            break;
        }
    }
}

/// Find the value following a named flag in the argument vector, e.g.
/// `--resume <id>`. Returns `None` when the flag is absent or has nothing
/// after it.
fn find_flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// Pull the plain text out of one stdin turn line, in the shape
/// `build_user_turn_line` in `src/backend/claude_cli/process.rs` produces.
/// Returns `None` for a line that does not parse or does not carry a text
/// content block, so a malformed line still gets an (empty) echoed reply
/// rather than killing the fake.
fn extract_user_text(line: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    value
        .get("message")?
        .get("content")?
        .as_array()?
        .iter()
        .find_map(|block| block.get("text").and_then(|t| t.as_str()))
        .map(|s| s.to_string())
}

/// Emit the startup `system`/`init` event, the only event this fake sends
/// before the first turn arrives on stdin.
fn emit_init(out: &mut impl Write, session_id: &str) {
    let event = serde_json::json!({
        "type": "system",
        "subtype": "init",
        "session_id": session_id,
        "cwd": ".",
        "tools": []
    });
    write_line(out, &event);
}

/// Emit one turn's worth of events: a `status` line, a three-part
/// `stream_event` sequence carrying a `text_delta` that echoes `text`
/// prefixed with `echo: `, and the terminal `result` event
/// `ClaudeCliDriver::send` waits on.
fn emit_turn(out: &mut impl Write, session_id: &str, text: &str) {
    let reply = format!("echo: {text}");

    write_line(
        out,
        &serde_json::json!({
            "type": "system",
            "subtype": "status",
            "status": "requesting"
        }),
    );
    write_line(
        out,
        &serde_json::json!({
            "type": "stream_event",
            "event": {
                "type": "content_block_start",
                "index": 0,
                "content_block": {"type": "text", "text": ""}
            }
        }),
    );
    write_line(
        out,
        &serde_json::json!({
            "type": "stream_event",
            "event": {
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "text_delta", "text": reply}
            }
        }),
    );
    write_line(
        out,
        &serde_json::json!({
            "type": "stream_event",
            "event": {"type": "content_block_stop", "index": 0}
        }),
    );
    write_line(
        out,
        &serde_json::json!({
            "type": "result",
            "is_error": false,
            "subtype": "success",
            "session_id": session_id,
            "result": reply
        }),
    );
}

fn write_line(out: &mut impl Write, value: &serde_json::Value) {
    let _ = writeln!(out, "{value}");
    let _ = out.flush();
}
