//! Wake-word phrase matching for [`super::service::VoiceService`]'s
//! [`crate::config::settings::TriggerMode::WakeWord`] mode.
//!
//! [`WakeWordMatcher`] turns a whisper transcript into a wake/no-wake
//! decision, plus the command that followed the phrase in the same
//! utterance if there was one ("hey deepseek what is the weather" ->
//! wake, command "what is the weather"). Matching is case-insensitive,
//! strips punctuation, and tolerates a small amount of whisper mis-hearing
//! so "hey deep seek" and "hey, deep-seek." both count as the phrase.

/// Max Levenshtein distance, on the wake phrase with spaces and
/// punctuation stripped, that still counts as a match. "hey deepseek"
/// normalizes to "heydeepseek" (11 characters). A distance of 2 absorbs
/// whisper's common single-word slips. "deepseek" heard as "deepsick" or
/// "deepseak" is a 2-substitution edit. It still stays tight: "hey there"
/// normalizes to "heythere", well past 2 edits from "heydeepseek". So
/// ordinary conversation does not fire the wake word by accident.
const MAX_EDIT_DISTANCE: usize = 2;

/// How many extra whitespace-delimited words beyond the wake phrase's own
/// word count are tried as a possible match prefix. Whisper sometimes
/// splits one word of the phrase into two ("deepseek" heard as "deep
/// seek"), which needs one extra transcript word to still line up.
const EXTRA_WORDS_FOR_SPLIT: usize = 1;

/// Result of a successful wake-phrase match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeMatch {
    /// Text left in the utterance after the wake phrase, if any
    /// ("hey deepseek what is the weather" -> "what is the weather").
    /// `None` when the utterance was only the wake phrase itself.
    pub command: Option<String>,
}

/// Matches whisper transcripts against a configured wake phrase.
pub struct WakeWordMatcher {
    wake_words: Vec<String>,
}

impl WakeWordMatcher {
    /// Build a matcher for `wake_phrase` (e.g. "hey deepseek").
    pub fn new(wake_phrase: &str) -> Self {
        let wake_words = wake_phrase
            .split_whitespace()
            .map(normalize_word)
            .filter(|w| !w.is_empty())
            .collect();
        Self { wake_words }
    }

    /// Check `transcript` for the wake phrase at its start. Returns the
    /// match, with any trailing command text, or `None` if the phrase is
    /// not there (ordinary speech that should be ignored).
    pub fn match_utterance(&self, transcript: &str) -> Option<WakeMatch> {
        if self.wake_words.is_empty() {
            return None;
        }
        let words: Vec<&str> = transcript.split_whitespace().collect();
        let wake_concat = self.wake_words.concat();
        let max_words = self.wake_words.len() + EXTRA_WORDS_FOR_SPLIT;
        let (count, distance) = best_prefix(&words, &wake_concat, max_words)?;
        if distance > MAX_EDIT_DISTANCE {
            return None;
        }
        let remainder = words[count..].join(" ");
        let command = if remainder.trim().is_empty() {
            None
        } else {
            Some(remainder)
        };
        Some(WakeMatch { command })
    }
}

/// Try consuming 1..=`max_words` leading words of `words`, normalizing and
/// concatenating each prefix, and measuring its edit distance to
/// `wake_concat`. Returns the word count and distance of the best
/// (lowest-distance) prefix tried, or `None` if `words` is empty.
fn best_prefix(words: &[&str], wake_concat: &str, max_words: usize) -> Option<(usize, usize)> {
    let limit = max_words.min(words.len());
    (1..=limit)
        .map(|count| {
            let concat: String = words[..count].iter().map(|w| normalize_word(w)).collect();
            (count, levenshtein(&concat, wake_concat))
        })
        .min_by_key(|&(_, distance)| distance)
}

/// Lowercase `word` and drop everything but ASCII alphanumerics, so
/// punctuation and casing never affect matching.
fn normalize_word(word: &str) -> String {
    word.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Levenshtein edit distance between two strings, in characters.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        curr[0] = i;
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
