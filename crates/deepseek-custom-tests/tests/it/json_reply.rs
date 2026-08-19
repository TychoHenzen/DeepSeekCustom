//! Unit tests for `deepseek_custom::json_reply` (`src/json_reply.rs`).
//!
//! The function had no direct test before it got a module of its own. Both
//! callers, `autopilot/answerer.rs` and `context/relevance.rs`, covered it
//! only through their own parse tests, and neither of those feeds it a
//! closing bracket that precedes the opening one.

use deepseek_custom::json_reply::extract_array_span;

#[test]
fn finds_a_bare_array() {
    assert_eq!(extract_array_span("[1, 2, 3]"), Some("[1, 2, 3]"));
}

#[test]
fn finds_an_array_wrapped_in_prose() {
    let reply = "Here are the scores you asked for:\n[{\"id\":0}]\nHope that helps.";
    assert_eq!(extract_array_span(reply), Some("[{\"id\":0}]"));
}

#[test]
fn finds_an_array_inside_a_markdown_fence() {
    let reply = "```json\n[{\"id\":0,\"score\":0.5}]\n```";
    assert_eq!(
        extract_array_span(reply),
        Some("[{\"id\":0,\"score\":0.5}]")
    );
}

#[test]
fn takes_the_outermost_span_when_arrays_nest() {
    assert_eq!(extract_array_span("[[1], [2]]"), Some("[[1], [2]]"));
}

#[test]
fn rejects_a_closing_bracket_before_the_opening_one() {
    assert_eq!(extract_array_span("] not an array ["), None);
}

#[test]
fn rejects_text_with_no_brackets() {
    assert_eq!(extract_array_span("I could not answer that."), None);
}

#[test]
fn rejects_an_unclosed_array() {
    assert_eq!(extract_array_span("[1, 2, 3"), None);
}
