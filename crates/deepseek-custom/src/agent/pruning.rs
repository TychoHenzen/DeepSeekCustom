//! Three-tier context pruning over a conversation's message vector.
//!
//! Pure and offline: no client, no network, no async. `MessageHistory` in
//! `history.rs` owns the token budget and delegates the actual work here.
//!
//! A turn group is one `Role::User` message plus every message that follows
//! it, up to (not including) the next `Role::User` message. Messages before
//! the first `Role::User` message form a leading group. Groups are always
//! derived fresh from the message vector, never cached, so there is no
//! parallel metadata to keep in sync.
//!
//! Pruning runs three tiers in order, cheapest first: elide image parts and
//! tool bodies (images first), collapse groups down to their user message
//! and final reply, then drop groups outright. Each tier stops the moment
//! the token budget is met, and never touches the last two groups.

use crate::agent::history::estimate_message_tokens;
use crate::api::types::{Content, ContentPart, Message, Role};

const ELIDED_PREFIX: &str = "[elided:";
/// `pub`, not private: moved out to `deepseek-custom-tests` in the
/// workspace split, its own tests compare against this marker directly.
pub const ELIDED_IMAGE_MARKER: &str = "[elided: image]";

/// What a prune pass changed. The agent loop logs this.
#[derive(Debug, Clone, PartialEq)]
pub struct PruneReport {
    pub tokens_before: usize,
    pub tokens_after: usize,
    pub images_elided: usize,
    pub tool_bodies_elided: usize,
    pub groups_collapsed: usize,
    pub groups_dropped: usize,
}

impl PruneReport {
    fn no_op(tokens: usize) -> Self {
        Self {
            tokens_before: tokens,
            tokens_after: tokens,
            images_elided: 0,
            tool_bodies_elided: 0,
            groups_collapsed: 0,
            groups_dropped: 0,
        }
    }
}

/// A turn group as a half-open range of message indices: [start, end).
///
/// `pub`, not private: `compute_groups`, which returns `Vec<Group>`, is
/// itself pub for the workspace split's moved tests.
pub type Group = (usize, usize);

/// Prune `messages` until the total token count (system prompt and suffix
/// tokens in `base_tokens`, plus every message) is at or below
/// `low_water_tokens`, or until nothing prunable is left. Runs tier 1
/// (elide tool bodies), then tier 2 (collapse groups), then tier 3 (drop
/// groups), stopping as soon as the budget is met.
///
/// `scores` is parallel to `messages`. Each score sits in 0.0..=1.0, lower
/// means less useful. A `None` value, or a slice with the wrong length,
/// falls back to a uniform score. Ordering then falls back to oldest-first.
pub fn prune_to_budget(
    messages: &mut Vec<Message>,
    base_tokens: usize,
    low_water_tokens: usize,
    scores: Option<&[f32]>,
) -> PruneReport {
    let tokens_before = total_tokens(base_tokens, messages);
    if tokens_before <= low_water_tokens {
        return PruneReport::no_op(tokens_before);
    }

    let mut eff_scores = effective_scores(messages.len(), scores);
    let mut report = PruneReport {
        tokens_before,
        tokens_after: tokens_before,
        images_elided: 0,
        tool_bodies_elided: 0,
        groups_collapsed: 0,
        groups_dropped: 0,
    };

    let (images_elided, tool_bodies_elided) =
        run_tier1(messages, &eff_scores, base_tokens, low_water_tokens);
    report.images_elided = images_elided;
    report.tool_bodies_elided = tool_bodies_elided;
    if total_tokens(base_tokens, messages) > low_water_tokens {
        report.groups_collapsed =
            run_tier2(messages, &mut eff_scores, base_tokens, low_water_tokens);
    }
    if total_tokens(base_tokens, messages) > low_water_tokens {
        report.groups_dropped = run_tier3(messages, &mut eff_scores, base_tokens, low_water_tokens);
    }

    report.tokens_after = total_tokens(base_tokens, messages);
    report
}

/// Scan for `Role::User` boundaries and return the resulting turn groups.
///
/// `pub`, not private: moved out to `deepseek-custom-tests` in the
/// workspace split, its own tests need this reachable from there.
pub fn compute_groups(messages: &[Message]) -> Vec<Group> {
    let mut groups = Vec::new();
    let mut start = 0;
    for (i, m) in messages.iter().enumerate() {
        if m.role == Role::User && i != start {
            groups.push((start, i));
            start = i;
        }
    }
    if start < messages.len() {
        groups.push((start, messages.len()));
    }
    groups
}

/// The last two groups are pinned. No tier may touch them. Returns the
/// index of the first pinned group. That index also counts the groups
/// eligible for pruning.
fn pinned_start(group_count: usize) -> usize {
    group_count.saturating_sub(2)
}

/// Scores to use for this pass: the caller's slice if present and the
/// right length, otherwise a uniform score for every message.
fn effective_scores(message_count: usize, scores: Option<&[f32]>) -> Vec<f32> {
    match scores {
        Some(s) if s.len() == message_count => s.to_vec(),
        _ => vec![0.0; message_count],
    }
}

/// A group's score: the mean of its members' scores.
fn group_score(group: Group, eff_scores: &[f32]) -> f32 {
    let (start, end) = group;
    if end <= start {
        return 0.0;
    }
    let sum: f32 = eff_scores[start..end].iter().sum();
    sum / (end - start) as f32
}

/// `pub`, not private: moved out to `deepseek-custom-tests` in the
/// workspace split, its own tests need this reachable from there.
pub fn total_tokens(base_tokens: usize, messages: &[Message]) -> usize {
    base_tokens + messages.iter().map(estimate_message_tokens).sum::<usize>()
}

/// Tier 1: first elide image parts, then replace tool bodies with a
/// placeholder. Both passes order lowest score first and oldest first on a
/// tie. Skips content already elided so a second pass does not double-wrap.
/// Leaves role, tool_call_id, and position alone. Returns
/// `(images_elided, tool_bodies_elided)`.
///
/// Images go first because they never prune well and are expensive: an
/// image part is either fully present or fully gone, and a single
/// screenshot's base64 payload can outweigh a lot of text. Dropping it
/// before touching any tool body reclaims the most budget for the least
/// structural damage.
fn run_tier1(
    messages: &mut [Message],
    eff_scores: &[f32],
    base_tokens: usize,
    low_water_tokens: usize,
) -> (usize, usize) {
    let groups = compute_groups(messages);
    let pinned = pinned_start(groups.len());
    let non_pinned: Vec<usize> = groups[..pinned].iter().flat_map(|&(s, e)| s..e).collect();

    let images_elided = elide_images(
        messages,
        eff_scores,
        &non_pinned,
        base_tokens,
        low_water_tokens,
    );

    let tool_bodies_elided = if total_tokens(base_tokens, messages) > low_water_tokens {
        elide_tool_bodies(
            messages,
            eff_scores,
            &non_pinned,
            base_tokens,
            low_water_tokens,
        )
    } else {
        0
    };

    (images_elided, tool_bodies_elided)
}

/// Replace every `ContentPart::ImageUrl` in `indices` with a text marker,
/// lowest score first and oldest first on a tie (message index first, part
/// index second). Stops the moment the budget is met. Once elided, a part
/// is a `Text` marker rather than an `ImageUrl`, so a second pass finds no
/// image part left to touch and cannot double-wrap it.
fn elide_images(
    messages: &mut [Message],
    eff_scores: &[f32],
    indices: &[usize],
    base_tokens: usize,
    low_water_tokens: usize,
) -> usize {
    let mut candidates: Vec<(usize, usize)> = Vec::new();
    for &idx in indices {
        if let Some(Content::Parts(parts)) = &messages[idx].content {
            for (part_idx, part) in parts.iter().enumerate() {
                if matches!(part, ContentPart::ImageUrl { .. }) {
                    candidates.push((idx, part_idx));
                }
            }
        }
    }
    candidates.sort_by(|&(a_idx, a_part), &(b_idx, b_part)| {
        eff_scores[a_idx]
            .partial_cmp(&eff_scores[b_idx])
            .unwrap()
            .then(a_idx.cmp(&b_idx))
            .then(a_part.cmp(&b_part))
    });

    let mut elided = 0;
    for (msg_idx, part_idx) in candidates {
        if total_tokens(base_tokens, messages) <= low_water_tokens {
            break;
        }
        if let Some(Content::Parts(parts)) = &mut messages[msg_idx].content {
            parts[part_idx] = ContentPart::Text {
                text: ELIDED_IMAGE_MARKER.into(),
            };
        }
        elided += 1;
    }
    elided
}

/// Replace tool bodies with a placeholder, lowest score first and oldest
/// first on a tie. Skips messages already elided so a second pass does not
/// double-wrap. Leaves role, tool_call_id, and position alone.
fn elide_tool_bodies(
    messages: &mut [Message],
    eff_scores: &[f32],
    indices: &[usize],
    base_tokens: usize,
    low_water_tokens: usize,
) -> usize {
    let mut candidates: Vec<usize> = indices
        .iter()
        .copied()
        .filter(|&idx| is_elidable(&messages[idx]))
        .collect();
    candidates.sort_by(|&a, &b| {
        eff_scores[a]
            .partial_cmp(&eff_scores[b])
            .unwrap()
            .then(a.cmp(&b))
    });

    let mut elided = 0;
    for idx in candidates {
        if total_tokens(base_tokens, messages) <= low_water_tokens {
            break;
        }
        let n = messages[idx]
            .content
            .as_ref()
            .and_then(Content::as_text)
            .expect("is_elidable checked content is Some text")
            .chars()
            .count();
        messages[idx].content = Some(Content::text(format!("[elided: {n} chars of tool output]")));
        elided += 1;
    }
    elided
}

fn is_elidable(msg: &Message) -> bool {
    msg.role == Role::Tool
        && msg
            .content
            .as_ref()
            .and_then(Content::as_text)
            .is_some_and(|c| !c.starts_with(ELIDED_PREFIX))
}

/// Tier 2: collapse non-pinned groups. Order is lowest group score first,
/// oldest first on a tie. Collapsing keeps the group's leading message.
/// It also keeps the last `Role::Assistant` message with content and no
/// tool calls. Every other message in the group is removed, including any
/// `Role::Tool` results. Groups never disappear in this tier because the
/// leading message always survives. So re-deriving groups fresh after
/// each collapse still finds every remaining group at the same ordinal
/// position.
fn run_tier2(
    messages: &mut Vec<Message>,
    eff_scores: &mut Vec<f32>,
    base_tokens: usize,
    low_water_tokens: usize,
) -> usize {
    let groups = compute_groups(messages);
    let pinned = pinned_start(groups.len());
    let order = priority_order(&groups, pinned, eff_scores);

    let mut collapsed = 0;
    for group_num in order {
        if total_tokens(base_tokens, messages) <= low_water_tokens {
            break;
        }
        let fresh_groups = compute_groups(messages);
        let (start, end) = fresh_groups[group_num];
        if collapse_group(messages, eff_scores, start, end) {
            collapsed += 1;
        }
    }
    collapsed
}

/// Tier 3: drop non-pinned groups outright, same ordering as tier 2.
/// Dropping removes a whole group. That shifts every later group's
/// ordinal position down by one. So the pending order is adjusted after
/// each drop instead of being recomputed from scratch.
fn run_tier3(
    messages: &mut Vec<Message>,
    eff_scores: &mut Vec<f32>,
    base_tokens: usize,
    low_water_tokens: usize,
) -> usize {
    let groups = compute_groups(messages);
    let pinned = pinned_start(groups.len());
    let mut order = priority_order(&groups, pinned, eff_scores);

    let mut dropped = 0;
    let mut i = 0;
    while i < order.len() {
        if total_tokens(base_tokens, messages) <= low_water_tokens {
            break;
        }
        let pos = order[i];
        let fresh_groups = compute_groups(messages);
        let (start, end) = fresh_groups[pos];
        messages.drain(start..end);
        eff_scores.drain(start..end);
        dropped += 1;
        for slot in order.iter_mut() {
            if *slot > pos {
                *slot -= 1;
            }
        }
        i += 1;
    }
    dropped
}

/// Non-pinned group ordinal numbers (0-indexed, left to right), sorted by
/// group score ascending, then ordinal number ascending on a tie.
fn priority_order(groups: &[Group], pinned: usize, eff_scores: &[f32]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..pinned).collect();
    order.sort_by(|&a, &b| {
        group_score(groups[a], eff_scores)
            .partial_cmp(&group_score(groups[b], eff_scores))
            .unwrap()
            .then(a.cmp(&b))
    });
    order
}

fn is_collapse_keeper(msg: &Message) -> bool {
    msg.role == Role::Assistant && msg.content.is_some() && msg.tool_calls.is_none()
}

/// Remove every message in `[start, end)` except the leading message and
/// the last message satisfying `is_collapse_keeper`. Returns whether
/// anything was removed (a no-op on an already-collapsed group).
fn collapse_group(
    messages: &mut Vec<Message>,
    eff_scores: &mut Vec<f32>,
    start: usize,
    end: usize,
) -> bool {
    if end - start <= 1 {
        return false;
    }
    let keep_assistant = (start + 1..end)
        .rev()
        .find(|&i| is_collapse_keeper(&messages[i]));

    let mut to_remove: Vec<usize> = (start..end)
        .filter(|&i| i != start && Some(i) != keep_assistant)
        .collect();
    if to_remove.is_empty() {
        return false;
    }
    to_remove.sort_unstable_by(|a, b| b.cmp(a));
    for idx in to_remove {
        messages.remove(idx);
        eff_scores.remove(idx);
    }
    true
}
