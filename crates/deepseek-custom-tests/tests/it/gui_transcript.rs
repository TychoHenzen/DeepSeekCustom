//! Unit tests for `deepseek_custom::gui::transcript` (`src/gui/transcript.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use deepseek_custom::agent::events::{
    RouteHop, RoutedEvent, StreamEvent, SubagentId, SubagentMeta,
};
use deepseek_custom::api::types::ImageAttachment;
use deepseek_custom::gui::transcript::{
    Block, BlockId, BlockKind, Severity, Span, SubagentState, Transcript,
};

#[test]
fn ids_stay_stable_across_appends() {
    let mut transcript = Transcript::new();
    let first = transcript.push(BlockKind::User {
        text: "hello".into(),
    });
    let second = transcript.push(BlockKind::User {
        text: "world".into(),
    });
    assert_ne!(first, second);
    assert_eq!(
        transcript.find(first).unwrap().kind,
        BlockKind::User {
            text: "hello".into()
        }
    );
    assert_eq!(
        transcript.find(second).unwrap().kind,
        BlockKind::User {
            text: "world".into()
        }
    );
}

#[test]
fn span_coalescing_merges_same_kind_text() {
    let mut transcript = Transcript::new();
    let id = transcript.push(BlockKind::Assistant { spans: Vec::new() });
    transcript.push_span(id, Span::Text("hel".into()));
    transcript.push_span(id, Span::Text("lo".into()));
    let BlockKind::Assistant { spans } = &transcript.find(id).unwrap().kind else {
        panic!("expected an Assistant block");
    };
    assert_eq!(spans, &[Span::Text("hello".into())]);
}

#[test]
fn span_coalescing_merges_same_kind_reasoning() {
    let mut transcript = Transcript::new();
    let id = transcript.push(BlockKind::Assistant { spans: Vec::new() });
    transcript.push_span(id, Span::Reasoning("think".into()));
    transcript.push_span(id, Span::Reasoning("ing".into()));
    let BlockKind::Assistant { spans } = &transcript.find(id).unwrap().kind else {
        panic!("expected an Assistant block");
    };
    assert_eq!(spans, &[Span::Reasoning("thinking".into())]);
}

#[test]
fn span_coalescing_does_not_merge_across_kinds() {
    let mut transcript = Transcript::new();
    let id = transcript.push(BlockKind::Assistant { spans: Vec::new() });
    transcript.push_span(id, Span::Text("said".into()));
    transcript.push_span(id, Span::Reasoning("thought".into()));
    transcript.push_span(id, Span::Text("said again".into()));
    let BlockKind::Assistant { spans } = &transcript.find(id).unwrap().kind else {
        panic!("expected an Assistant block");
    };
    assert_eq!(
        spans,
        &[
            Span::Text("said".into()),
            Span::Reasoning("thought".into()),
            Span::Text("said again".into()),
        ]
    );
}

#[test]
fn tool_call_fills_in_place() {
    let mut transcript = Transcript::new();
    let id = transcript.push(BlockKind::ToolCall {
        tool: "Bash".into(),
        args: "ls".into(),
        output: None,
        is_error: false,
    });
    transcript.complete_tool_call(id, "file1\nfile2".into(), false);
    assert_eq!(
        transcript.find(id).unwrap().kind,
        BlockKind::ToolCall {
            tool: "Bash".into(),
            args: "ls".into(),
            output: Some("file1\nfile2".into()),
            is_error: false,
        }
    );
}

#[test]
fn lookup_for_missing_id_returns_none() {
    let mut transcript = Transcript::new();
    let id = transcript.push(BlockKind::User {
        text: "hello".into(),
    });
    transcript.clear();
    assert!(transcript.find(id).is_none());
}

#[test]
fn clear_empties_the_transcript() {
    let mut transcript = Transcript::new();
    transcript.push(BlockKind::User {
        text: "hello".into(),
    });
    transcript.push(BlockKind::Notice {
        text: "Session reset".into(),
        severity: Severity::Info,
    });
    transcript.clear();
    assert!(transcript.blocks().is_empty());
}

#[test]
fn push_span_on_missing_id_does_nothing() {
    let mut transcript = Transcript::new();
    transcript.push_span(BlockId::new_for_test(999), Span::Text("x".into()));
    assert!(transcript.blocks().is_empty());
}

#[test]
fn complete_tool_call_on_non_tool_block_does_nothing() {
    let mut transcript = Transcript::new();
    let id = transcript.push(BlockKind::User {
        text: "hello".into(),
    });
    transcript.complete_tool_call(id, "output".into(), true);
    assert_eq!(
        transcript.find(id).unwrap().kind,
        BlockKind::User {
            text: "hello".into()
        }
    );
}

#[test]
fn plain_text_turn_produces_one_assistant_block_with_one_span() {
    let mut transcript = Transcript::new();
    transcript.apply_stream_event(StreamEvent::Text {
        turn: 1,
        text: "hel".into(),
    });
    transcript.apply_stream_event(StreamEvent::Text {
        turn: 1,
        text: "lo".into(),
    });
    transcript.apply_stream_event(StreamEvent::TurnEnd {
        turn: 1,
        finish_reason: "stop".into(),
        total_tokens: 10,
        prompt_cache_hit_tokens: 0,
        prompt_cache_miss_tokens: 0,
    });
    let blocks = transcript.blocks();
    assert_eq!(blocks.len(), 1);
    assert_eq!(
        blocks[0].kind,
        BlockKind::Assistant {
            spans: vec![Span::Text("hello".into())],
        }
    );
}

#[test]
fn interleaved_text_and_reasoning_stay_in_arrival_order() {
    let mut transcript = Transcript::new();
    transcript.apply_stream_event(StreamEvent::Reasoning {
        turn: 1,
        text: "thinking".into(),
    });
    transcript.apply_stream_event(StreamEvent::Text {
        turn: 1,
        text: "said".into(),
    });
    transcript.apply_stream_event(StreamEvent::Reasoning {
        turn: 1,
        text: "more".into(),
    });
    let blocks = transcript.blocks();
    assert_eq!(blocks.len(), 1);
    assert_eq!(
        blocks[0].kind,
        BlockKind::Assistant {
            spans: vec![
                Span::Reasoning("thinking".into()),
                Span::Text("said".into()),
                Span::Reasoning("more".into()),
            ],
        }
    );
}

#[test]
fn tool_call_produces_assistant_tool_call_then_new_assistant() {
    let mut transcript = Transcript::new();
    transcript.apply_stream_event(StreamEvent::Text {
        turn: 1,
        text: "before".into(),
    });
    transcript.apply_stream_event(StreamEvent::ToolCallStart {
        turn: 1,
        tool: "Bash".into(),
        args: "ls".into(),
    });
    transcript.apply_stream_event(StreamEvent::ToolCallEnd {
        turn: 1,
        tool: "Bash".into(),
        output: "file1".into(),
        is_error: false,
    });
    transcript.apply_stream_event(StreamEvent::Text {
        turn: 1,
        text: "after".into(),
    });
    let blocks = transcript.blocks();
    assert_eq!(blocks.len(), 3);
    assert_eq!(
        blocks[0].kind,
        BlockKind::Assistant {
            spans: vec![Span::Text("before".into())],
        }
    );
    assert_eq!(
        blocks[1].kind,
        BlockKind::ToolCall {
            tool: "Bash".into(),
            args: "ls".into(),
            output: Some("file1".into()),
            is_error: false,
        }
    );
    assert_eq!(
        blocks[2].kind,
        BlockKind::Assistant {
            spans: vec![Span::Text("after".into())],
        }
    );
}

#[test]
fn tool_call_end_sets_is_error() {
    let mut transcript = Transcript::new();
    transcript.apply_stream_event(StreamEvent::ToolCallStart {
        turn: 1,
        tool: "Bash".into(),
        args: "bad-command".into(),
    });
    transcript.apply_stream_event(StreamEvent::ToolCallEnd {
        turn: 1,
        tool: "Bash".into(),
        output: "command not found".into(),
        is_error: true,
    });
    let blocks = transcript.blocks();
    assert_eq!(
        blocks[0].kind,
        BlockKind::ToolCall {
            tool: "Bash".into(),
            args: "bad-command".into(),
            output: Some("command not found".into()),
            is_error: true,
        }
    );
}

#[test]
fn interrupt_produces_a_notice_block_with_interrupt_severity() {
    let mut transcript = Transcript::new();
    transcript.apply_stream_event(StreamEvent::Interrupted {
        message: "Interrupted by user".into(),
    });
    let blocks = transcript.blocks();
    assert_eq!(blocks.len(), 1);
    assert_eq!(
        blocks[0].kind,
        BlockKind::Notice {
            text: "Interrupted by user".into(),
            severity: Severity::Warning,
        }
    );
}

#[test]
fn a_tool_call_block_starts_collapsed() {
    let mut transcript = Transcript::new();
    transcript.apply_stream_event(StreamEvent::ToolCallStart {
        turn: 1,
        tool: "Bash".into(),
        args: "ls".into(),
    });
    assert!(transcript.blocks()[0].collapsed);
}

#[test]
fn a_user_block_starts_uncollapsed() {
    let mut transcript = Transcript::new();
    transcript.push(BlockKind::User {
        text: "hello".into(),
    });
    assert!(!transcript.blocks()[0].collapsed);
}

#[test]
fn set_collapsed_flips_the_stored_flag() {
    let mut transcript = Transcript::new();
    let id = transcript.push(BlockKind::User {
        text: "hello".into(),
    });
    transcript.set_collapsed(id, true);
    assert!(transcript.find(id).unwrap().collapsed);
    transcript.set_collapsed(id, false);
    assert!(!transcript.find(id).unwrap().collapsed);
}

#[test]
fn set_collapsed_on_a_missing_id_does_nothing() {
    let mut transcript = Transcript::new();
    transcript.push(BlockKind::User {
        text: "hello".into(),
    });
    transcript.set_collapsed(BlockId::new_for_test(999), true);
    assert!(!transcript.blocks()[0].collapsed);
}

#[test]
fn two_turns_produce_separate_assistant_blocks() {
    let mut transcript = Transcript::new();
    transcript.apply_stream_event(StreamEvent::Text {
        turn: 1,
        text: "first".into(),
    });
    transcript.apply_stream_event(StreamEvent::TurnEnd {
        turn: 1,
        finish_reason: "stop".into(),
        total_tokens: 5,
        prompt_cache_hit_tokens: 0,
        prompt_cache_miss_tokens: 0,
    });
    transcript.apply_stream_event(StreamEvent::Text {
        turn: 2,
        text: "second".into(),
    });
    let blocks = transcript.blocks();
    assert_eq!(blocks.len(), 2);
    assert_eq!(
        blocks[0].kind,
        BlockKind::Assistant {
            spans: vec![Span::Text("first".into())],
        }
    );
    assert_eq!(
        blocks[1].kind,
        BlockKind::Assistant {
            spans: vec![Span::Text("second".into())],
        }
    );
    assert_ne!(blocks[0].id, blocks[1].id);
}

#[test]
fn an_image_block_round_trips_through_json() {
    let mut transcript = Transcript::new();
    let id = transcript.push(BlockKind::Image {
        image: ImageAttachment {
            data: "AAA".into(),
            media_type: "image/png".into(),
        },
    });

    let json = serde_json::to_string(&transcript).unwrap();
    let restored: Transcript = serde_json::from_str(&json).unwrap();

    assert_eq!(
        restored.find(id).unwrap().kind,
        BlockKind::Image {
            image: ImageAttachment {
                data: "AAA".into(),
                media_type: "image/png".into(),
            },
        }
    );
}

/// A session file saved before this variant existed has no `"Image"`
/// key anywhere in it. It must still load, and the loaded transcript
/// must still be able to take new blocks afterward.
#[test]
fn an_old_session_file_with_no_image_block_still_loads() {
    let json =
        r#"{"blocks":[{"id":3,"collapsed":false,"kind":{"User":{"text":"hi"}}}],"next_id":4}"#;
    let mut restored: Transcript = serde_json::from_str(json).unwrap();
    assert_eq!(restored.blocks().len(), 1);
    let new_id = restored.push(BlockKind::Image {
        image: ImageAttachment {
            data: "BBB".into(),
            media_type: "image/jpeg".into(),
        },
    });
    assert_eq!(restored.blocks().len(), 2);
    assert_ne!(new_id, BlockId::new_for_test(3));
}

#[test]
fn round_trips_one_block_of_every_kind_through_json() {
    let mut transcript = Transcript::new();
    transcript.push(BlockKind::User { text: "hi".into() });
    transcript.push(BlockKind::Assistant {
        spans: vec![Span::Text("said".into()), Span::Reasoning("thought".into())],
    });
    transcript.push(BlockKind::ToolCall {
        tool: "Bash".into(),
        args: "ls".into(),
        output: Some("file1".into()),
        is_error: false,
    });
    transcript.push(BlockKind::ToolCall {
        tool: "Bash".into(),
        args: "ls".into(),
        output: None,
        is_error: false,
    });
    transcript.push(BlockKind::Notice {
        text: "warn".into(),
        severity: Severity::Warning,
    });
    transcript.push(BlockKind::Notice {
        text: "err".into(),
        severity: Severity::Error,
    });
    transcript.push(BlockKind::Image {
        image: ImageAttachment {
            data: "AAA".into(),
            media_type: "image/png".into(),
        },
    });

    let json = serde_json::to_string(&transcript).unwrap();
    let restored: Transcript = serde_json::from_str(&json).unwrap();

    let original_kinds: Vec<&BlockKind> = transcript.blocks().iter().map(|b| &b.kind).collect();
    let restored_kinds: Vec<&BlockKind> = restored.blocks().iter().map(|b| &b.kind).collect();
    assert_eq!(original_kinds, restored_kinds);
    let original_ids: Vec<BlockId> = transcript.blocks().iter().map(|b| b.id).collect();
    let restored_ids: Vec<BlockId> = restored.blocks().iter().map(|b| b.id).collect();
    assert_eq!(original_ids, restored_ids);
}

#[test]
fn appending_after_deserialize_never_collides_with_a_loaded_id() {
    let mut transcript = Transcript::new();
    transcript.push(BlockKind::User { text: "a".into() });
    transcript.push(BlockKind::User { text: "b".into() });
    let json = serde_json::to_string(&transcript).unwrap();
    let mut restored: Transcript = serde_json::from_str(&json).unwrap();

    let loaded_ids: Vec<BlockId> = restored.blocks().iter().map(|b| b.id).collect();
    let new_id = restored.push(BlockKind::User { text: "c".into() });
    assert!(!loaded_ids.contains(&new_id));
}

#[test]
fn a_bogus_next_id_in_the_json_still_yields_a_fresh_unused_id() {
    let json =
        r#"{"blocks":[{"id":5,"collapsed":false,"kind":{"User":{"text":"hi"}}}],"next_id":0}"#;
    let mut restored: Transcript = serde_json::from_str(json).unwrap();
    let new_id = restored.push(BlockKind::User {
        text: "next".into(),
    });
    assert_ne!(new_id, BlockId::new_for_test(5));
}

#[test]
fn a_deserialize_with_no_next_id_field_still_yields_a_fresh_unused_id() {
    let json = r#"{"blocks":[{"id":7,"collapsed":false,"kind":{"User":{"text":"hi"}}}]}"#;
    let mut restored: Transcript = serde_json::from_str(json).unwrap();
    let new_id = restored.push(BlockKind::User {
        text: "next".into(),
    });
    assert_ne!(new_id, BlockId::new_for_test(7));
}

#[test]
fn a_deserialized_transcript_has_no_open_assistant_block() {
    let mut transcript = Transcript::new();
    transcript.apply_stream_event(StreamEvent::Text {
        turn: 1,
        text: "first".into(),
    });
    let json = serde_json::to_string(&transcript).unwrap();
    let mut restored: Transcript = serde_json::from_str(&json).unwrap();

    restored.apply_stream_event(StreamEvent::Text {
        turn: 2,
        text: "second".into(),
    });
    let blocks = restored.blocks();
    assert_eq!(blocks.len(), 2);
    assert_eq!(
        blocks[0].kind,
        BlockKind::Assistant {
            spans: vec![Span::Text("first".into())],
        }
    );
    assert_eq!(
        blocks[1].kind,
        BlockKind::Assistant {
            spans: vec![Span::Text("second".into())],
        }
    );
}

#[test]
fn a_new_block_starts_unpinned() {
    let mut transcript = Transcript::new();
    let id = transcript.push(BlockKind::User {
        text: "hello".into(),
    });
    assert!(!transcript.find(id).unwrap().pinned);
}

#[test]
fn set_pinned_by_path_flips_a_top_level_block() {
    let mut transcript = Transcript::new();
    let id = transcript.push(BlockKind::User {
        text: "hello".into(),
    });
    transcript.set_pinned_by_path(&[id], true);
    assert!(transcript.find(id).unwrap().pinned);
    transcript.set_pinned_by_path(&[id], false);
    assert!(!transcript.find(id).unwrap().pinned);
}

#[test]
fn find_mut_by_path_on_a_missing_top_level_id_returns_none() {
    let mut transcript = Transcript::new();
    assert!(
        transcript
            .find_mut_by_path(&[BlockId::new_for_test(999)])
            .is_none()
    );
}

fn test_hop(id: SubagentId, depth: u32) -> RouteHop {
    test_hop_with_counts(id, depth, 1, 20, 0, 10)
}

fn test_hop_with_counts(
    id: SubagentId,
    depth: u32,
    session_turns: u32,
    session_turn_cap: u32,
    send_message_calls: u32,
    send_message_call_cap: u32,
) -> RouteHop {
    RouteHop {
        id,
        meta: SubagentMeta {
            backend: "ollama".into(),
            model: "test-model".into(),
            depth,
        },
        session_turns,
        session_turn_cap,
        send_message_calls,
        send_message_call_cap,
    }
}

fn subagent_kind(block: &Block) -> (&str, &str, u32, SubagentState) {
    let BlockKind::Subagent {
        backend,
        model,
        depth,
        state,
        ..
    } = &block.kind
    else {
        panic!("expected a Subagent block");
    };
    (backend.as_str(), model.as_str(), *depth, *state)
}

#[test]
fn an_empty_route_behaves_exactly_like_apply_stream_event() {
    let mut transcript = Transcript::new();
    transcript.apply_routed_event(RoutedEvent::own(StreamEvent::Text {
        turn: 1,
        text: "hello".into(),
    }));
    let blocks = transcript.blocks();
    assert_eq!(blocks.len(), 1);
    assert_eq!(
        blocks[0].kind,
        BlockKind::Assistant {
            spans: vec![Span::Text("hello".into())],
        }
    );
}

#[test]
fn a_one_element_route_creates_a_subagent_block_and_fills_its_inner_transcript() {
    let mut transcript = Transcript::new();
    let id = SubagentId::next();
    transcript.apply_routed_event(RoutedEvent {
        route: vec![test_hop(id, 1)],
        event: StreamEvent::Text {
            turn: 1,
            text: "hi from subagent".into(),
        },
    });

    let blocks = transcript.blocks();
    assert_eq!(blocks.len(), 1);
    let (backend, model, depth, state) = subagent_kind(&blocks[0]);
    assert_eq!(backend, "ollama");
    assert_eq!(model, "test-model");
    assert_eq!(depth, 1);
    assert_eq!(state, SubagentState::Running);

    let BlockKind::Subagent { transcript, .. } = &blocks[0].kind else {
        panic!("expected a Subagent block");
    };
    assert_eq!(
        transcript.blocks()[0].kind,
        BlockKind::Assistant {
            spans: vec![Span::Text("hi from subagent".into())],
        }
    );
}

/// The counts on a `RouteHop` land on the `Subagent` block's own
/// fields, and a later event for the same subagent with different
/// counts overwrites them: the block reflects the most recent hop, not
/// just the one that created it. This is what makes the counts live
/// across a `SendMessage` follow-up rather than frozen at whatever the
/// session's opening turn reported.
#[test]
fn subagent_counts_travel_from_the_route_hop_onto_the_block_and_stay_live() {
    let mut transcript = Transcript::new();
    let id = SubagentId::next();
    transcript.apply_routed_event(RoutedEvent {
        route: vec![test_hop_with_counts(id, 1, 1, 20, 0, 10)],
        event: StreamEvent::Text {
            turn: 1,
            text: "opening turn".into(),
        },
    });
    let (turns, turn_cap, calls, call_cap) = subagent_counts(&transcript.blocks()[0]);
    assert_eq!((turns, turn_cap, calls, call_cap), (1, 20, 0, 10));

    transcript.apply_routed_event(RoutedEvent {
        route: vec![test_hop_with_counts(id, 1, 2, 20, 1, 10)],
        event: StreamEvent::Text {
            turn: 2,
            text: "follow up".into(),
        },
    });
    let (turns, turn_cap, calls, call_cap) = subagent_counts(&transcript.blocks()[0]);
    assert_eq!((turns, turn_cap, calls, call_cap), (2, 20, 1, 10));
}

fn subagent_counts(block: &Block) -> (u32, u32, u32, u32) {
    let BlockKind::Subagent {
        session_turns,
        session_turn_cap,
        send_message_calls,
        send_message_call_cap,
        ..
    } = &block.kind
    else {
        panic!("expected a Subagent block");
    };
    (
        *session_turns,
        *session_turn_cap,
        *send_message_calls,
        *send_message_call_cap,
    )
}

#[test]
fn a_turn_end_marks_the_subagent_block_done() {
    let mut transcript = Transcript::new();
    let id = SubagentId::next();
    transcript.apply_routed_event(RoutedEvent {
        route: vec![test_hop(id, 1)],
        event: StreamEvent::Text {
            turn: 1,
            text: "hi".into(),
        },
    });
    transcript.apply_routed_event(RoutedEvent {
        route: vec![test_hop(id, 1)],
        event: StreamEvent::TurnEnd {
            turn: 1,
            finish_reason: "stop".into(),
            total_tokens: 5,
            prompt_cache_hit_tokens: 0,
            prompt_cache_miss_tokens: 0,
        },
    });
    let (_, _, _, state) = subagent_kind(&transcript.blocks()[0]);
    assert_eq!(state, SubagentState::Done);
}

#[test]
fn an_error_marks_the_subagent_block_failed() {
    let mut transcript = Transcript::new();
    let id = SubagentId::next();
    transcript.apply_routed_event(RoutedEvent {
        route: vec![test_hop(id, 1)],
        event: StreamEvent::Error {
            message: "boom".into(),
        },
    });
    let (_, _, _, state) = subagent_kind(&transcript.blocks()[0]);
    assert_eq!(state, SubagentState::Failed);
}

#[test]
fn an_interrupt_marks_the_subagent_block_interrupted() {
    let mut transcript = Transcript::new();
    let id = SubagentId::next();
    transcript.apply_routed_event(RoutedEvent {
        route: vec![test_hop(id, 1)],
        event: StreamEvent::Interrupted {
            message: "stopped".into(),
        },
    });
    let (_, _, _, state) = subagent_kind(&transcript.blocks()[0]);
    assert_eq!(state, SubagentState::Interrupted);
}

#[test]
fn a_two_element_route_nests_a_subagent_block_inside_the_first() {
    let mut transcript = Transcript::new();
    let outer_id = SubagentId::next();
    let inner_id = SubagentId::next();
    transcript.apply_routed_event(RoutedEvent {
        route: vec![test_hop(outer_id, 1), test_hop(inner_id, 2)],
        event: StreamEvent::Text {
            turn: 1,
            text: "hi from nested".into(),
        },
    });

    let blocks = transcript.blocks();
    assert_eq!(blocks.len(), 1);
    let (_, _, outer_depth, outer_state) = subagent_kind(&blocks[0]);
    assert_eq!(outer_depth, 1);
    // The outer dispatch is still running: only the innermost hop that
    // an event is native to advances a block's own state.
    assert_eq!(outer_state, SubagentState::Running);

    let BlockKind::Subagent {
        transcript: outer_transcript,
        ..
    } = &blocks[0].kind
    else {
        panic!("expected a Subagent block");
    };
    let inner_blocks = outer_transcript.blocks();
    assert_eq!(inner_blocks.len(), 1);
    let (_, _, inner_depth, _) = subagent_kind(&inner_blocks[0]);
    assert_eq!(inner_depth, 2);

    let BlockKind::Subagent {
        transcript: inner_transcript,
        ..
    } = &inner_blocks[0].kind
    else {
        panic!("expected a nested Subagent block");
    };
    assert_eq!(
        inner_transcript.blocks()[0].kind,
        BlockKind::Assistant {
            spans: vec![Span::Text("hi from nested".into())],
        }
    );
}

#[test]
fn a_two_element_path_reaches_a_block_nested_inside_a_subagent() {
    let mut transcript = Transcript::new();
    let outer_id = SubagentId::next();
    let inner_id = SubagentId::next();
    transcript.apply_routed_event(RoutedEvent {
        route: vec![test_hop(outer_id, 1), test_hop(inner_id, 2)],
        event: StreamEvent::Text {
            turn: 1,
            text: "hi from nested".into(),
        },
    });

    let outer_block_id = transcript.blocks()[0].id;
    let BlockKind::Subagent {
        transcript: outer_transcript,
        ..
    } = &transcript.blocks()[0].kind
    else {
        panic!("expected a Subagent block");
    };
    let inner_block_id = outer_transcript.blocks()[0].id;

    let path = [outer_block_id, inner_block_id];
    transcript.set_pinned_by_path(&path, true);
    transcript.set_collapsed_by_path(&path, false);

    let BlockKind::Subagent {
        transcript: outer_transcript,
        ..
    } = &transcript.blocks()[0].kind
    else {
        panic!("expected a Subagent block");
    };
    let inner_block = &outer_transcript.blocks()[0];
    assert!(inner_block.pinned);
    assert!(!inner_block.collapsed);
}

#[test]
fn two_sibling_subagents_at_the_same_level_do_not_collide() {
    let mut transcript = Transcript::new();
    let first_id = SubagentId::next();
    let second_id = SubagentId::next();
    transcript.apply_routed_event(RoutedEvent {
        route: vec![test_hop(first_id, 1)],
        event: StreamEvent::Text {
            turn: 1,
            text: "from first".into(),
        },
    });
    transcript.apply_routed_event(RoutedEvent {
        route: vec![test_hop(second_id, 1)],
        event: StreamEvent::Text {
            turn: 1,
            text: "from second".into(),
        },
    });
    // A second event for the first subagent must land back in its own
    // block, not create a third one.
    transcript.apply_routed_event(RoutedEvent {
        route: vec![test_hop(first_id, 1)],
        event: StreamEvent::Text {
            turn: 1,
            text: " again".into(),
        },
    });

    let blocks = transcript.blocks();
    assert_eq!(blocks.len(), 2);
    let BlockKind::Subagent {
        transcript: first_transcript,
        ..
    } = &blocks[0].kind
    else {
        panic!("expected a Subagent block");
    };
    assert_eq!(
        first_transcript.blocks()[0].kind,
        BlockKind::Assistant {
            spans: vec![Span::Text("from first again".into())],
        }
    );
    let BlockKind::Subagent {
        transcript: second_transcript,
        ..
    } = &blocks[1].kind
    else {
        panic!("expected a Subagent block");
    };
    assert_eq!(
        second_transcript.blocks()[0].kind,
        BlockKind::Assistant {
            spans: vec![Span::Text("from second".into())],
        }
    );
}

#[test]
fn a_transcript_with_a_nested_subagent_block_round_trips_through_json() {
    let mut transcript = Transcript::new();
    let outer_id = SubagentId::next();
    let inner_id = SubagentId::next();
    transcript.apply_routed_event(RoutedEvent {
        route: vec![test_hop(outer_id, 1), test_hop(inner_id, 2)],
        event: StreamEvent::Text {
            turn: 1,
            text: "nested text".into(),
        },
    });
    transcript.apply_routed_event(RoutedEvent {
        route: vec![test_hop(outer_id, 1), test_hop(inner_id, 2)],
        event: StreamEvent::TurnEnd {
            turn: 1,
            finish_reason: "stop".into(),
            total_tokens: 3,
            prompt_cache_hit_tokens: 0,
            prompt_cache_miss_tokens: 0,
        },
    });

    let json = serde_json::to_string(&transcript).unwrap();
    let restored: Transcript = serde_json::from_str(&json).unwrap();

    let blocks = restored.blocks();
    assert_eq!(blocks.len(), 1);
    let (backend, model, depth, _) = subagent_kind(&blocks[0]);
    assert_eq!(backend, "ollama");
    assert_eq!(model, "test-model");
    assert_eq!(depth, 1);

    let BlockKind::Subagent {
        transcript: inner_transcript,
        ..
    } = &blocks[0].kind
    else {
        panic!("expected a Subagent block");
    };
    let inner_blocks = inner_transcript.blocks();
    assert_eq!(inner_blocks.len(), 1);
    let (_, _, inner_depth, inner_state) = subagent_kind(&inner_blocks[0]);
    assert_eq!(inner_depth, 2);
    assert_eq!(inner_state, SubagentState::Done);
}

#[test]
fn repeat_iteration_start_closes_open_assistant_and_pushes_notice() {
    let mut transcript = Transcript::new();
    transcript.apply_stream_event(StreamEvent::Text {
        turn: 1,
        text: "first iteration text".into(),
    });
    transcript.apply_stream_event(StreamEvent::RepeatIterationStart {
        index: 2,
        total: 5,
        task: "run the plan".into(),
    });
    transcript.apply_stream_event(StreamEvent::Text {
        turn: 2,
        text: "second iteration text".into(),
    });
    let blocks = transcript.blocks();
    assert_eq!(blocks.len(), 4);
    assert_eq!(
        blocks[1].kind,
        BlockKind::Notice {
            text: "Iteration 2 of 5".into(),
            severity: Severity::Info,
        }
    );
    assert_eq!(
        blocks[0].kind,
        BlockKind::Assistant {
            spans: vec![Span::Text("first iteration text".into())],
        }
    );
    assert_eq!(
        blocks[3].kind,
        BlockKind::Assistant {
            spans: vec![Span::Text("second iteration text".into())],
        }
    );
}

#[test]
fn repeat_iteration_start_draws_the_task_as_a_user_block() {
    let mut transcript = Transcript::new();
    transcript.apply_stream_event(StreamEvent::RepeatIterationStart {
        index: 1,
        total: 3,
        task: "tighten the codebase".into(),
    });
    let blocks = transcript.blocks();
    assert_eq!(blocks.len(), 2);
    assert_eq!(
        blocks[1].kind,
        BlockKind::User {
            text: "tighten the codebase".into(),
        }
    );
}

#[test]
fn repeat_finished_pushes_notice() {
    let mut transcript = Transcript::new();
    transcript.apply_stream_event(StreamEvent::RepeatFinished {
        completed: 3,
        total: 5,
    });
    let blocks = transcript.blocks();
    assert_eq!(blocks.len(), 1);
    assert_eq!(
        blocks[0].kind,
        BlockKind::Notice {
            text: "Autopilot finished: 3 of 5 iterations".into(),
            severity: Severity::Info,
        }
    );
}
