//! Plain-language readability metrics.
//!
//! `Api`-only: these run on replies the harness produces itself, not on
//! replies from a `claude_cli` child process.

/// Compute the Flesch-Kincaid Grade Level for `text`.
///
/// Returns a grade level as a float (e.g. 8.0 means an 8th-grade reading
/// level). The formula is:
///
/// ```text
/// 0.39 * (words / sentences) + 11.8 * (syllables / words) - 15.59
/// ```
///
/// Hand-written rather than pulled in as a dependency.  Splits on `.!?`
/// for sentences, whitespace for words, and a vowel-group rule for
/// syllables.
///
/// Edge cases: empty input, zero words, or zero sentences all return 0.0.
pub fn flesch_kincaid_grade(text: &str) -> f32 {
    let text = text.trim();
    if text.is_empty() {
        return 0.0;
    }

    let sentence_count = count_sentences(text).max(1) as f32;
    let word_count = count_words(text);
    if word_count == 0 {
        return 0.0;
    }
    let word_count = word_count as f32;
    let syllable_count = count_syllables(text) as f32;

    0.39 * (word_count / sentence_count) + 11.8 * (syllable_count / word_count) - 15.59
}

fn count_sentences(text: &str) -> usize {
    text.chars().filter(|c| matches!(c, '.' | '!' | '?')).count()
}

fn count_words(text: &str) -> usize {
    text.split_whitespace().count()
}

fn count_syllables(text: &str) -> usize {
    text.split_whitespace().map(syllables_in_word).sum()
}

fn syllables_in_word(word: &str) -> usize {
    // Strip punctuation from the ends (except for internal apostrophes
    // and hyphens, which syllable counters usually ignore or treat as
    // boundaries).
    let word = word.trim_matches(|c: char| !c.is_alphabetic() && c != '\'' && c != '-');
    if word.is_empty() {
        return 1; // a punctuation-only token, treat as one syllable
    }

    let lower = word.to_lowercase();
    let chars: Vec<char> = lower.chars().collect();

    let mut count = 0usize;
    let mut prev_vowel = false;
    let vowels: &[char] = &['a', 'e', 'i', 'o', 'u', 'y'];

    for (i, &ch) in chars.iter().enumerate() {
        let is_vowel = vowels.contains(&ch);
        if is_vowel && !prev_vowel {
            count += 1;
        }
        prev_vowel = is_vowel;
        // 'y' counts as a vowel only when not at the start of the word
        // (common heuristic). This is handled above by including 'y'
        // in the vowel list for counting groups; at the start of a word
        // it is usually a consonant. We fix that here.
        if i == 0 && ch == 'y' && count > 0 {
            count -= 1;
        }
    }

    // Silent 'e' at the end: if the word ends with 'e' (and is not just
    // "e"), and has more than one syllable, deduct one.
    if count > 1 && chars.len() > 1 && chars.last() == Some(&'e') {
        count -= 1;
    }

    // Every word has at least one syllable.
    count.max(1)
}

