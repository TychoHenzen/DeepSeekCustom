//! Unit tests for `deepseek_custom::tools::line_endings`
//! (`src/tools/line_endings.rs`).

use deepseek_custom::tools::line_endings::{has_crlf, to_crlf, to_lf};

#[test]
fn finds_windows_line_endings_only_when_they_are_there() {
    assert!(has_crlf("one\r\ntwo"));
    assert!(!has_crlf("one\ntwo"));
    assert!(!has_crlf("no newline at all"));
    assert!(!has_crlf("a bare \r carriage return"));
}

#[test]
fn reduces_every_crlf_to_a_bare_lf() {
    assert_eq!(to_lf("one\r\ntwo\r\n"), "one\ntwo\n");
    assert_eq!(to_lf("already\nlf\n"), "already\nlf\n");
    assert_eq!(to_lf("mixed\r\nand\nplain\r\n"), "mixed\nand\nplain\n");
}

#[test]
fn writes_every_line_ending_as_crlf() {
    assert_eq!(to_crlf("one\ntwo\n"), "one\r\ntwo\r\n");
    assert_eq!(to_crlf("mixed\r\nand\n"), "mixed\r\nand\r\n");
}

#[test]
fn converting_to_crlf_twice_changes_nothing_the_second_time() {
    let once = to_crlf("one\ntwo\n");
    assert_eq!(to_crlf(&once), once);
}

#[test]
fn a_bare_carriage_return_is_left_alone() {
    // A lone `\r` is not a line ending either helper claims to handle, so
    // it must survive a round trip rather than turning into a newline.
    assert_eq!(to_lf("a\rb"), "a\rb");
    assert_eq!(to_crlf("a\rb"), "a\rb");
}
