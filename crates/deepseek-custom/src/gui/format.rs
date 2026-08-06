//! Pure formatting and color mapping for transcript blocks.
//! No egui runtime dependency needed. All functions are free and
//! stateless, testable without a window.

use std::time::Instant;

use eframe::egui::Color32;

use super::transcript::{
    Block, BlockKind, Severity, Span, SubagentState,
};

/// Tool calls and notices that carry an error use this colour.
pub const TOOL_ERROR_COLOR: Color32 =
    Color32::from_rgb(255, 80, 80);

/// Image label colour in the transcript.
pub const IMAGE_LABEL_COLOR: Color32 =
    Color32::from_rgb(180, 220, 255);

/// Vertical space before a turn boundary (a User block after
/// the first).
pub const TURN_GAP: f32 = 16.0;

/// Vertical space between ordinary blocks.
pub const BLOCK_GAP: f32 = 4.0;

/// Map a notice severity to its display colour.
pub fn severity_color(severity: Severity) -> Color32 {
    match severity {
        Severity::Error => Color32::from_rgb(255, 80, 80),
        Severity::Warning => Color32::from_rgb(255, 180, 60),
        Severity::Info => Color32::from_rgb(120, 200, 255),
        Severity::Debug => Color32::from_rgb(150, 150, 150),
    }
}

/// Human-readable role label for a block kind.
pub fn role_label(kind: &BlockKind) -> &'static str {
    match kind {
        BlockKind::User { .. } => "You",
        BlockKind::Assistant { .. } => "Assistant",
        BlockKind::ToolCall { .. } => "Tool",
        BlockKind::Notice { .. } => "Notice",
        BlockKind::Image { .. } => "Image",
        BlockKind::Subagent { .. } => "Subagent",
    }
}

/// Vertical gap before the block at `index`.
pub fn gap_before(index: usize, kind: &BlockKind) -> f32 {
    if index == 0 {
        return BLOCK_GAP;
    }
    if matches!(kind, BlockKind::User { .. }) {
        return TURN_GAP;
    }
    BLOCK_GAP
}

/// Flatten whitespace and cut to `max_chars`, appending "...".
pub fn truncate_args(args: &str, max_chars: usize) -> String {
    let flat: String =
        args.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max_chars {
        return flat;
    }
    let cut: String = flat.chars().take(max_chars).collect();
    format!("{cut}...")
}

/// One-line tool call summary with a collapse marker.
pub fn tool_summary(
    collapsed: bool,
    tool: &str,
    args: &str,
    is_error: bool,
) -> String {
    let marker = if collapsed { "\u{25b6}" } else { "\u{25bc}" };
    let err = if is_error { " [error]" } else { "" };
    let short = truncate_args(args, 60);
    format!("{marker} {tool} {short}{err}")
}

/// Colour for a tool call header line.
pub fn tool_color(is_error: bool) -> Color32 {
    if is_error {
        TOOL_ERROR_COLOR
    } else {
        Color32::from_rgb(200, 200, 100)
    }
}

/// Colour for a tool call output text.
pub fn tool_output_color(is_error: bool) -> Color32 {
    if is_error {
        TOOL_ERROR_COLOR
    } else {
        Color32::from_rgb(180, 180, 180)
    }
}

/// Colour for a block in raw-output mode.
pub fn block_color(kind: &BlockKind) -> Color32 {
    match kind {
        BlockKind::User { .. } => Color32::from_rgb(100, 200, 255),
        BlockKind::Assistant { .. } => Color32::WHITE,
        BlockKind::ToolCall { is_error, .. } => tool_color(*is_error),
        BlockKind::Notice { severity, .. } => severity_color(*severity),
        BlockKind::Image { .. } => IMAGE_LABEL_COLOR,
        BlockKind::Subagent { .. } => Color32::from_rgb(180, 180, 255),
    }
}

/// Plain text for a single span (raw-output mode).
pub fn raw_span_text(span: &Span) -> String {
    match span {
        Span::Text(t) => t.clone(),
        Span::Reasoning(t) => format!("[reasoning] {t}"),
    }
}

/// Plain text for a whole block (raw-output mode).
pub fn raw_block_text(block: &Block) -> String {
    match &block.kind {
        BlockKind::User { text } => format!("You: {text}"),
        BlockKind::Assistant { spans } => {
            let lines: Vec<String> =
                spans.iter().map(raw_span_text).collect();
            format!("Assistant:\n{}", lines.join("\n"))
        }
        BlockKind::ToolCall {
            tool, args, output, is_error,
        } => raw_tool(tool, args, output.as_deref(), *is_error),
        BlockKind::Notice { text, .. } => text.clone(),
        BlockKind::Image { image } => {
            let len = image.data.len();
            format!(
                "[image: {}, {len} bytes base64]",
                image.media_type,
            )
        }
        BlockKind::Subagent {
            backend, model, depth, state, elapsed_ms,
            started_at, transcript, session_turns,
            session_turn_cap, send_message_calls,
            send_message_call_cap, ..
        } => raw_subagent(
            backend, model, *depth, *state, *started_at,
            *elapsed_ms, transcript.blocks(),
            *session_turns, *session_turn_cap,
            *send_message_calls, *send_message_call_cap,
        ),
    }
}

fn raw_tool(
    tool: &str,
    args: &str,
    output: Option<&str>,
    is_error: bool,
) -> String {
    let err = if is_error { " [error]" } else { "" };
    let out = output.unwrap_or("(running)");
    format!("\u{2699} {tool} {args}{err} \u{2192} {out}")
}

#[allow(clippy::too_many_arguments)]
fn raw_subagent(
    backend: &str,
    model: &str,
    depth: u32,
    state: SubagentState,
    started_at: Option<Instant>,
    stored_ms: u64,
    blocks: &[Block],
    turns: u32,
    turn_cap: u32,
    calls: u32,
    call_cap: u32,
) -> String {
    let ms = subagent_elapsed_ms(started_at, stored_ms);
    let header = subagent_header_summary(
        backend, model, depth, state, ms, turns, turn_cap,
        calls, call_cap,
    );
    let body: Vec<String> = blocks.iter().map(|b| {
        raw_block_text(b)
            .lines()
            .map(|l| format!("  {l}"))
            .collect::<Vec<_>>()
            .join("\n")
    }).collect();
    if body.is_empty() {
        return header;
    }
    format!("{header}\n{}", body.join("\n"))
}

/// Live or stored elapsed time for a subagent block.
pub fn subagent_elapsed_ms(
    started_at: Option<Instant>,
    stored_ms: u64,
) -> u64 {
    match started_at {
        Some(start) => start.elapsed().as_millis() as u64,
        None => stored_ms,
    }
}

/// Colour for a subagent state badge.
pub fn subagent_state_color(state: SubagentState) -> Color32 {
    match state {
        SubagentState::Running => Color32::from_rgb(100, 200, 255),
        SubagentState::Done => Color32::from_rgb(100, 255, 100),
        SubagentState::Failed => Color32::from_rgb(255, 80, 80),
        SubagentState::Interrupted => Color32::from_rgb(255, 180, 60),
    }
}

/// Format milliseconds as `"{:.1}s"`.
pub fn format_elapsed_ms(ms: u64) -> String {
    format!("{:.1}s", ms as f64 / 1000.0)
}

/// Full header line for a subagent block.
#[allow(clippy::too_many_arguments)]
pub fn subagent_header_summary(
    backend: &str,
    model: &str,
    depth: u32,
    state: SubagentState,
    elapsed_ms: u64,
    session_turns: u32,
    session_turn_cap: u32,
    send_message_calls: u32,
    send_message_call_cap: u32,
) -> String {
    let badge = match state {
        SubagentState::Running => "RUNNING",
        SubagentState::Done => "DONE",
        SubagentState::Failed => "FAILED",
        SubagentState::Interrupted => "INTERRUPTED",
    };
    let elapsed = format_elapsed_ms(elapsed_ms);
    format!(
        "[{badge}] {backend}/{model} depth={depth} {elapsed} \
         turns {session_turns}/{session_turn_cap} \
         sends {send_message_calls}/{send_message_call_cap}"
    )
}
