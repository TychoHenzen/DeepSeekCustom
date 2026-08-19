//! Finding the JSON array inside a model's text reply.
//!
//! Lives at the crate root because its two callers share no parent but the
//! crate: `src/autopilot/answerer.rs` parses a policy answer array, and
//! `src/context/relevance.rs` parses a relevance score array. Both ask a
//! model for a JSON array and both get back prose or a markdown fence
//! around it often enough that neither can call `serde_json` on the raw
//! reply. Each held a byte-identical copy of this function before this
//! module existed.

/// Find the outermost `[...]` span in `text`.
pub fn extract_array_span(text: &str) -> Option<&str> {
    let start = text.find('[')?;
    let end = text.rfind(']')?;
    if end < start {
        return None;
    }
    Some(&text[start..=end])
}
