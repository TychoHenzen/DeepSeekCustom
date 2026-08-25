//! Strict parsing for the final text returned by a frontier backend.

use super::{PatchCandidate, PatchEnvelopeError, decode_patch_envelope};

/// Parse frontier final text without stripping commentary or guessing at a patch.
///
/// The shared decoder requires exactly one JSON document, so code fences,
/// leading prose, trailing prose, and a second JSON object all fail here.
pub fn decode_frontier_patch_output(
    final_text: &str,
) -> Result<PatchCandidate, PatchEnvelopeError> {
    decode_patch_envelope(final_text)
}
