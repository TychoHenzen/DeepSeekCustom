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
//! Pruning runs three tiers in order, cheapest first: elide tool bodies,
//! collapse groups down to their user message and final reply, then drop
//! groups outright. Each tier stops the moment the token budget is met, and
//! never touches the last two groups.

use crate::agent::history::estimate_message_tokens;
use crate::api::types::{Message, Role};

const ELIDED_PREFIX: &str = "[elided:";

/// What a prune pass changed. The agent loop logs this.
#[derive(Debug, Clone, PartialEq)]
pub struct PruneReport {
    pub tokens_before: usize,
    pub tokens_after: usize,
    pub tool_bodies_elided: usize,
    pub groups_collapsed: usize,
    pub groups_dropped: usize,
}

impl PruneReport {
    fn no_op(tokens: usize) -> Self {
        Self {
            tokens_before: tokens,
            tokens_after: tokens,
            tool_bodies_elided: 0,
            groups_collapsed: 0,
            groups_dropped: 0,
        }
    }
}

/// A turn group as a half-open range of message indices: [start, end).
type Group = (usize, usize);

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
        tool_bodies_elided: 0,
        groups_collapsed: 0,
        groups_dropped: 0,
    };

    report.tool_bodies_elided = run_tier1(messages, &eff_scores, base_tokens, low_water_tokens);
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
fn compute_groups(messages: &[Message]) -> Vec<Group> {
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

fn total_tokens(base_tokens: usize, messages: &[Message]) -> usize {
    base_tokens + messages.iter().map(estimate_message_tokens).sum::<usize>()
}

/// Tier 1: replace tool bodies with a placeholder, lowest score first and
/// oldest first on a tie. Skips messages already elided so a second pass
/// does not double-wrap. Leaves role, tool_call_id, and position alone.
fn run_tier1(
    messages: &mut [Message],
    eff_scores: &[f32],
    base_tokens: usize,
    low_water_tokens: usize,
) -> usize {
    let groups = compute_groups(messages);
    let pinned = pinned_start(groups.len());
    let mut candidates: Vec<usize> = groups[..pinned]
        .iter()
        .flat_map(|&(s, e)| s..e)
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
            .expect("is_elidable checked content is Some")
            .chars()
            .count();
        messages[idx].content = Some(format!("[elided: {n} chars of tool output]"));
        elided += 1;
    }
    elided
}

fn is_elidable(msg: &Message) -> bool {
    msg.role == Role::Tool
        && msg
            .content
            .as_deref()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn user(s: &str) -> Message {
        Message::user(s.into())
    }
    fn assistant(s: &str) -> Message {
        Message::assistant(s.into())
    }
    fn tool(id: &str, s: &str) -> Message {
        Message::tool_result(id.into(), s.into())
    }
    fn assistant_with_call(id: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: None,
            tool_calls: Some(vec![crate::api::types::ToolCall {
                id: id.into(),
                call_type: "function".into(),
                function: Some(crate::api::types::FunctionCall {
                    name: Some("bash".into()),
                    arguments: Some("{}".into()),
                }),
                index: None,
            }]),
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    /// A tool-using turn: user prompt, an assistant tool call, its tool
    /// result, and a final assistant reply with no tool calls.
    fn tool_turn(user_text: &str, call_id: &str, tool_body: &str, reply: &str) -> Vec<Message> {
        vec![
            user(user_text),
            assistant_with_call(call_id),
            tool(call_id, tool_body),
            assistant(reply),
        ]
    }

    /// `Message` has no `PartialEq` (types.rs is out of scope for this
    /// step), so compare the fields these tests actually set.
    fn message_snapshot(m: &Message) -> (Role, Option<String>, Option<String>) {
        (m.role.clone(), m.content.clone(), m.tool_call_id.clone())
    }
    fn messages_eq(a: &[Message], b: &[Message]) -> bool {
        a.len() == b.len()
            && a.iter()
                .zip(b.iter())
                .all(|(x, y)| message_snapshot(x) == message_snapshot(y))
    }

    fn many_turns(n: usize) -> Vec<Message> {
        let mut out = Vec::new();
        for i in 0..n {
            out.extend(tool_turn(
                &format!("question {i}"),
                &format!("call{i}"),
                &"x".repeat(200),
                &format!("answer {i}"),
            ));
        }
        out
    }

    #[test]
    fn noop_when_already_under_low_water() {
        let mut messages = vec![user("hi"), assistant("hello")];
        let before = total_tokens(0, &messages);
        let report = prune_to_budget(&mut messages, 0, before + 1000, None);
        assert_eq!(report.tokens_before, before);
        assert_eq!(report.tokens_after, before);
        assert_eq!(report.tool_bodies_elided, 0);
        assert_eq!(report.groups_collapsed, 0);
        assert_eq!(report.groups_dropped, 0);
        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn tier1_elides_tool_body_and_keeps_id_and_position() {
        let mut messages = many_turns(4);
        // Force tier 1 to act, but stop before it touches structure.
        // The budget sits just below the full size. That is above
        // what eliding one message can reach, so the loop stops there.
        let full = total_tokens(0, &messages);
        let target = full - 10;
        let report = prune_to_budget(&mut messages, 0, target, None);
        assert!(report.tool_bodies_elided >= 1);
        // The eldest non-pinned tool message (index 2, "call0") is elided.
        let tool_msg = &messages[2];
        assert_eq!(tool_msg.role, Role::Tool);
        assert_eq!(tool_msg.tool_call_id.as_deref(), Some("call0"));
        assert!(tool_msg.content.as_deref().unwrap().starts_with("[elided:"));
    }

    #[test]
    fn eliding_is_idempotent() {
        let mut messages = many_turns(4);
        let full = total_tokens(0, &messages);
        let target = full - 10;
        prune_to_budget(&mut messages, 0, target, None);
        let elided_content = messages[2].content.clone();
        // A second pass at the same low water mark should not touch an
        // already-elided message again (no double [elided: [elided: ...).
        let report2 = prune_to_budget(&mut messages, 0, target, None);
        assert_eq!(messages[2].content, elided_content);
        assert!(
            !messages[2]
                .content
                .as_deref()
                .unwrap()
                .contains("[elided: [elided:")
        );
        // Whatever tier1 touched this round, it did not re-wrap index 2.
        let _ = report2;
    }

    #[test]
    fn wrong_length_scores_treated_as_none() {
        let mut a = many_turns(3);
        let mut b = a.clone();
        let full = total_tokens(0, &a);
        let target = full - 10;
        let bogus_scores = vec![0.9f32; a.len() - 1]; // wrong length on purpose
        let report_a = prune_to_budget(&mut a, 0, target, Some(&bogus_scores));
        let report_b = prune_to_budget(&mut b, 0, target, None);
        assert!(messages_eq(&a, &b));
        assert_eq!(report_a.tool_bodies_elided, report_b.tool_bodies_elided);
    }

    #[test]
    fn scores_none_orders_oldest_first() {
        let mut messages = many_turns(4);
        let full = total_tokens(0, &messages);
        // Budget just under full: exactly one tool body gets elided.
        let report = prune_to_budget(&mut messages, 0, full - 10, None);
        assert_eq!(report.tool_bodies_elided, 1);
        assert!(
            messages[2]
                .content
                .as_deref()
                .unwrap()
                .starts_with("[elided:")
        );
        // Every later tool body is untouched.
        assert_eq!(
            messages[6].content.as_deref(),
            Some("x".repeat(200)).as_deref()
        );
    }

    #[test]
    fn lower_scored_messages_elided_before_higher_scored() {
        // 4 turns, so both turn 0 and turn 1 (indices 0-7) are non-pinned.
        // Their tool bodies sit at index 2 and index 6.
        let mut messages = many_turns(4);
        // Score every message 0.5. Give the second turn's tool result
        // (index 6) a low score of 0.0 instead. It then goes first,
        // even though it is not the oldest.
        let mut scores = vec![0.5f32; messages.len()];
        scores[6] = 0.0;
        let full = total_tokens(0, &messages);
        let report = prune_to_budget(&mut messages, 0, full - 10, Some(&scores));
        assert_eq!(report.tool_bodies_elided, 1);
        assert!(
            messages[6]
                .content
                .as_deref()
                .unwrap()
                .starts_with("[elided:")
        );
        assert!(
            !messages[2]
                .content
                .as_deref()
                .unwrap()
                .starts_with("[elided:")
        );
    }

    #[test]
    fn last_two_groups_survive_every_tier() {
        let mut messages = many_turns(6);
        let pinned_snapshot = messages[16..24].to_vec();
        // Drive the budget down hard so all three tiers run.
        let report = prune_to_budget(&mut messages, 0, 1, None);
        assert!(report.groups_dropped > 0 || report.groups_collapsed > 0);
        let new_len = messages.len();
        assert!(messages_eq(&messages[new_len - 8..], &pinned_snapshot));
    }

    #[test]
    fn tool_call_pairing_holds_after_tier2() {
        let mut messages = many_turns(6);
        // Budget low enough to force collapsing but not so low tier 3
        // engages: pick a target between "everything" and "just pinned".
        let full = total_tokens(0, &messages);
        let pinned_tokens = total_tokens(0, &messages[16..]);
        let target = pinned_tokens + 20;
        let report = prune_to_budget(&mut messages, 0, target, None);
        assert!(target < full);
        assert!(report.groups_collapsed > 0 || report.groups_dropped > 0);

        for (i, m) in messages.iter().enumerate() {
            if m.role == Role::Tool {
                let id = m.tool_call_id.as_deref().expect("tool result has an id");
                let has_pair = messages[..i].iter().rev().take(1).any(|prev| {
                    prev.tool_calls
                        .as_ref()
                        .is_some_and(|tcs| tcs.iter().any(|tc| tc.id == id))
                });
                assert!(
                    has_pair,
                    "tool result at {i} has no preceding matching call"
                );
            }
        }
        for (i, m) in messages.iter().enumerate() {
            if let Some(tcs) = &m.tool_calls {
                for tc in tcs {
                    let has_result = messages[i + 1..]
                        .iter()
                        .take(1)
                        .any(|next| next.tool_call_id.as_deref() == Some(tc.id.as_str()));
                    assert!(
                        has_result,
                        "tool call {} at {i} has no matching result",
                        tc.id
                    );
                }
            }
        }
    }

    #[test]
    fn message_history_delegates_noop_to_pure_prune() {
        use crate::agent::history::MessageHistory;
        let mut h = MessageHistory::new("system".into());
        h.push(user("hi"));
        h.push(assistant("hello"));
        let before = h.estimated_tokens();
        let report = h.prune_to_budget(before + 1000, None);
        assert_eq!(report.tokens_before, before);
        assert_eq!(report.tokens_after, before);
        assert_eq!(h.estimated_tokens(), before);
        assert_eq!(h.len(), 2);
    }

    #[test]
    fn group_boundaries_include_leading_group() {
        let messages = vec![assistant("stray"), user("hi"), assistant("hello")];
        let groups = compute_groups(&messages);
        assert_eq!(groups, vec![(0, 1), (1, 3)]);
    }
}
