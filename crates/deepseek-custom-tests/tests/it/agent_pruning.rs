//! Unit tests for `deepseek_custom::agent::pruning`, moved out of the
//! production module as part of the two-crate workspace split.

use deepseek_custom::agent::history::MessageHistory;
use deepseek_custom::agent::pruning::{
    ELIDED_IMAGE_MARKER, compute_groups, prune_to_budget, total_tokens,
};
use deepseek_custom::api::types::{Content, ContentPart, FunctionCall, Message, Role, ToolCall};

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
        tool_calls: Some(vec![ToolCall {
            id: id.into(),
            call_type: "function".into(),
            function: Some(FunctionCall {
                name: Some("bash".into()),
                arguments: Some("{}".into()),
            }),
            index: None,
        }]),
        tool_call_id: None,
        reasoning_content: None,
    }
}

fn user_with_image(text: &str, url: &str) -> Message {
    Message {
        role: Role::User,
        content: Some(Content::Parts(vec![
            ContentPart::Text { text: text.into() },
            ContentPart::ImageUrl { url: url.into() },
        ])),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    }
}

fn image_only(url: &str) -> Message {
    Message {
        role: Role::User,
        content: Some(Content::Parts(vec![ContentPart::ImageUrl {
            url: url.into(),
        }])),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    }
}

/// A turn like `tool_turn`, but the leading user message carries both
/// text and an image part instead of plain text.
fn image_turn(
    user_text: &str,
    image_url: &str,
    call_id: &str,
    tool_body: &str,
    reply: &str,
) -> Vec<Message> {
    vec![
        user_with_image(user_text, image_url),
        assistant_with_call(call_id),
        tool(call_id, tool_body),
        assistant(reply),
    ]
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
fn message_snapshot(m: &Message) -> (Role, Option<Content>, Option<String>) {
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
    assert_eq!(report.images_elided, 0);
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
    assert!(
        tool_msg
            .content
            .as_ref()
            .and_then(Content::as_text)
            .unwrap()
            .starts_with("[elided:")
    );
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
            .as_ref()
            .and_then(Content::as_text)
            .unwrap()
            .contains("[elided: [elided:")
    );
    // Whatever tier1 touched this round, it did not re-wrap index 2.
    let _ = report2;
}

#[test]
fn tier1_elides_image_before_tool_body() {
    let image_url = format!("data:image/png;base64,{}", "A".repeat(4000));
    let mut messages = image_turn(
        "look at this",
        &image_url,
        "call0",
        &"x".repeat(200),
        "reply0",
    );
    messages.extend(many_turns(2));
    let full = total_tokens(0, &messages);

    // Compute the token total right after only the image is elided, so
    // the budget target lands exactly there and tier one has no reason
    // to go on to the tool body.
    let mut only_image_elided = messages.clone();
    if let Some(Content::Parts(parts)) = &mut only_image_elided[0].content {
        parts[1] = ContentPart::Text {
            text: ELIDED_IMAGE_MARKER.into(),
        };
    }
    let target = total_tokens(0, &only_image_elided);
    assert!(target < full);

    let report = prune_to_budget(&mut messages, 0, target, None);
    assert_eq!(report.images_elided, 1);
    assert_eq!(report.tool_bodies_elided, 0);

    match &messages[0].content {
        Some(Content::Parts(parts)) => assert_eq!(
            parts[1],
            ContentPart::Text {
                text: ELIDED_IMAGE_MARKER.into()
            }
        ),
        other => panic!("expected Parts content, got {other:?}"),
    }
    // The tool body is untouched: image elision alone met the budget.
    assert_eq!(
        messages[2].content.as_ref().and_then(Content::as_text),
        Some("x".repeat(200)).as_deref()
    );
}

#[test]
fn image_eliding_is_idempotent_and_then_falls_through_to_tool_bodies() {
    let image_url = format!("data:image/png;base64,{}", "A".repeat(4000));
    let mut messages = image_turn(
        "look at this",
        &image_url,
        "call0",
        &"x".repeat(200),
        "reply0",
    );
    messages.extend(many_turns(2));

    let mut only_image_elided = messages.clone();
    if let Some(Content::Parts(parts)) = &mut only_image_elided[0].content {
        parts[1] = ContentPart::Text {
            text: ELIDED_IMAGE_MARKER.into(),
        };
    }
    let target = total_tokens(0, &only_image_elided);

    let mut both_elided = only_image_elided.clone();
    let tool_chars = both_elided[2]
        .content
        .as_ref()
        .and_then(Content::as_text)
        .unwrap()
        .chars()
        .count();
    both_elided[2].content = Some(Content::text(format!(
        "[elided: {tool_chars} chars of tool output]"
    )));
    let target2 = total_tokens(0, &both_elided);
    assert!(target2 < target);

    let report1 = prune_to_budget(&mut messages, 0, target, None);
    assert_eq!(report1.images_elided, 1);
    let elided_image_content = messages[0].content.clone();

    // A second, lower-budget pass finds no image part left to touch
    // (it is now a Text marker, not an ImageUrl) and does not double
    // wrap it. It falls through to eliding the tool body instead.
    let report2 = prune_to_budget(&mut messages, 0, target2, None);
    assert_eq!(report2.images_elided, 0);
    assert_eq!(report2.tool_bodies_elided, 1);
    assert_eq!(messages[0].content, elided_image_content);
    match &messages[0].content {
        Some(Content::Parts(parts)) => match &parts[1] {
            ContentPart::Text { text } => {
                assert!(!text.contains("[elided: [elided:"));
            }
            other => panic!("expected an elided text marker, got {other:?}"),
        },
        other => panic!("expected Parts content, got {other:?}"),
    }
}

#[test]
fn image_only_message_elides_sanely() {
    let image_url = format!("data:image/png;base64,{}", "B".repeat(3000));
    let mut messages = vec![image_only(&image_url), assistant("ack")];
    messages.extend(many_turns(2));
    let full = total_tokens(0, &messages);

    let mut only_image_elided = messages.clone();
    if let Some(Content::Parts(parts)) = &mut only_image_elided[0].content {
        parts[0] = ContentPart::Text {
            text: ELIDED_IMAGE_MARKER.into(),
        };
    }
    let target = total_tokens(0, &only_image_elided);
    assert!(target < full);

    let report = prune_to_budget(&mut messages, 0, target, None);
    assert_eq!(report.images_elided, 1);
    assert_eq!(messages[0].role, Role::User);
    assert_eq!(
        messages[0].content,
        Some(Content::Parts(vec![ContentPart::Text {
            text: ELIDED_IMAGE_MARKER.into()
        }]))
    );
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
            .as_ref()
            .and_then(Content::as_text)
            .unwrap()
            .starts_with("[elided:")
    );
    // Every later tool body is untouched.
    assert_eq!(
        messages[6].content.as_ref().and_then(Content::as_text),
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
            .as_ref()
            .and_then(Content::as_text)
            .unwrap()
            .starts_with("[elided:")
    );
    assert!(
        !messages[2]
            .content
            .as_ref()
            .and_then(Content::as_text)
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
