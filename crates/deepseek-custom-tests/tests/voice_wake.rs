//! Unit tests for `deepseek_custom::voice::wake` (`src/voice/wake.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use deepseek_custom::voice::wake::{WakeMatch, WakeWordMatcher};

#[test]
fn exact_match_has_no_command() {
    let matcher = WakeWordMatcher::new("hey deepseek");
    let result = matcher.match_utterance("hey deepseek");
    assert_eq!(result, Some(WakeMatch { command: None }));
}

#[test]
fn matches_regardless_of_casing() {
    let matcher = WakeWordMatcher::new("hey deepseek");
    let result = matcher.match_utterance("Hey DeepSeek");
    assert_eq!(result, Some(WakeMatch { command: None }));
}

#[test]
fn matches_through_punctuation() {
    let matcher = WakeWordMatcher::new("hey deepseek");
    let result = matcher.match_utterance("hey, deepseek!");
    assert_eq!(result, Some(WakeMatch { command: None }));
}

#[test]
fn matches_deepseek_split_into_two_words() {
    let matcher = WakeWordMatcher::new("hey deepseek");
    let result = matcher.match_utterance("hey deep seek");
    assert_eq!(result, Some(WakeMatch { command: None }));
}

#[test]
fn near_miss_within_tolerance_still_matches() {
    // "deepsick" is a 2-substitution edit away from "deepseek"
    // (positions 5 and 6: e->i, e->c), right at MAX_EDIT_DISTANCE.
    let matcher = WakeWordMatcher::new("hey deepseek");
    let result = matcher.match_utterance("hey deepsick");
    assert_eq!(result, Some(WakeMatch { command: None }));
}

#[test]
fn phrase_just_outside_tolerance_does_not_match() {
    // "xyzpseek" is a 3-substitution edit away from "deepseek"
    // (positions 0-2: d->x, e->y, e->z), one past MAX_EDIT_DISTANCE.
    let matcher = WakeWordMatcher::new("hey deepseek");
    let result = matcher.match_utterance("hey xyzpseek");
    assert_eq!(result, None);
}

#[test]
fn unrelated_phrase_does_not_match() {
    let matcher = WakeWordMatcher::new("hey deepseek");
    let result = matcher.match_utterance("hey there");
    assert_eq!(result, None);
}

#[test]
fn command_is_extracted_from_remainder_of_utterance() {
    let matcher = WakeWordMatcher::new("hey deepseek");
    let result = matcher.match_utterance("hey deepseek what is the weather");
    assert_eq!(
        result,
        Some(WakeMatch {
            command: Some("what is the weather".to_string())
        })
    );
}

#[test]
fn empty_transcript_does_not_match() {
    let matcher = WakeWordMatcher::new("hey deepseek");
    let result = matcher.match_utterance("");
    assert_eq!(result, None);
}
