# Roadmap checklist, 2026-08-04

Derived from `2026-08-04-long-term-roadmap.md`. That document holds the
reasoning. This one holds the work items, in build order.

Decisions already settled:

- `settings.json` at the repo root is a save file. The GUI rewrites it on every
  control change. No document may claim what it contains. Treat it as volatile.
- `ThinkingStore` and `parse_thinking_tags` get deleted, not wired in.
- This batch runs the prework plus phase 1. Phases 2 through 6 stay unchecked.

## Prework

- [x] S01 Stop documenting the contents of `settings.json`. Drop the
      `default_backend` claim from `CLAUDE.md` and `AGENTS.md` and say the file
      is a volatile save file instead.
- [x] S02 Delete `ThinkingStore` and `parse_thinking_tags` from
      `src/context/mod.rs`, plus their tests.
- [x] S03 Add `wiremock` as a dev dependency and a `tests/` integration target.
      First test: a full turn against a canned SSE stream.
- [x] S04 Mock test: a turn that calls a tool and feeds the result back.
- [x] S05 Mock tests: a retry after a 500, a broken chunk, and a stream that
      ends without `[DONE]`.
- [x] S06 Move the voice tests behind a `voice-models` cargo feature, so a
      plain `cargo test` on a fresh checkout is green.

## Phase 1: a structured transcript

- [x] S07 New `src/gui/transcript.rs` holding `Transcript`, `Block`, `BlockId`,
      `BlockKind`, and `Span`. Plain data, no egui types, with unit tests.
- [x] S08 Map every `StreamEvent` to a block mutation on `Transcript`, tested
      from a scripted event sequence with no GUI.
- [x] S09 Replace `output_lines` with `Transcript` in `src/gui/mod.rs`.
      `handle_stream_event` writes blocks.
- [x] S10 Render blocks by kind: bubbles for user and assistant, dimmed and
      collapsed reasoning, a one-line tool call summary that expands.
- [x] S11 Rewrite the color-asserting GUI tests to assert on block kinds and
      spans. The raw-output toggle renders the same blocks as plain text.
- [x] S12 Update `CLAUDE.md` and `AGENTS.md` for the transcript model and the
      end of the color-as-type convention.

## Phase 2: subagent blocks

- [x] Add `RoutedEvent { route: Vec<SubagentId>, event: StreamEvent }`.
- [x] Replace `spawn_event_drain` with a forwarder that prepends its own id.
- [x] Resolve a route to a block and append to that block's inner transcript.
- [x] Render a `Subagent` block as a collapsing header with backend, model,
      depth, state badge, and elapsed time. Collapsed by default.
- [x] Settle whether a running subagent auto-expands. Settled as no: collapsed
      by default, a live one-line header status, and a pin button to hold one
      open. A fanout of five subagents must not push the main conversation
      off screen.

## Phase 3: multi-turn subagent sessions

- [x] `SubagentRegistry` holding live sessions keyed by `SubagentId`.
- [x] `Task` gains `keep_open` and returns a session id when it stays open.
- [x] `SendMessage { session_id, prompt }` tool.
- [x] `CloseSession { session_id }` tool.
- [x] Lifetime rule: a session dies with its parent's turn. `Reset` closes all.
- [x] Multi-turn `claude_cli` sessions run on `process.rs`, not `one_shot.rs`.
- [x] `may_dispatch` gates `SendMessage` the same way it gates `Task`.
- [x] Turn caps: per session and per parent turn, both shown in the header.
      Built in the same change, not after.
- [x] Stub backend for subagent tests (phase 7 piece 4).

## Phase 4: a switchable working directory

- [x] Split `project_root` from `working_dir` and name the split in the docs.
- [x] `working_dir` as `Arc<Mutex<PathBuf>>`, read per tool call.
- [x] `SystemPrompt` reads it each turn and stops calling `current_dir()`.
- [x] `ClaudeCliDriver` restarts its child on a change.
- [x] `Cd { path }` tool, returning a tool error for a bad target.
- [x] Directory field in the sidebar and current directory in the status bar.
- [x] `working_dir` override on `Task`.
- [x] Document that there is no path sandbox, on purpose.

## Phase 5: one effort control across every backend

- [x] `enum Effort { None, Low, Medium, High, Max }`.
- [x] Per-provider mapping in `prepare_request`. Check the `claude_cli` mapping
      against the real binary before writing it.
- [x] Sidebar control replaces the thinking checkbox. `Arc<AtomicU8>` flag.
- [x] Delete `thinking_flag` and the `thinking.enabled` settings block.
- [x] Optional `effort` field on `Task`.
- [x] Mock test: every level produces a different request on each backend.

## Phase 6: image input

- [x] `Content` and `ContentPart` enums, serializing text-only messages exactly
      as they serialize today.
- [x] Check image support per backend with a real request before mapping.
- [x] Say so in the transcript when a backend cannot take an image.
- [x] Ctrl+V paste and drag and drop into a pending attachment strip.
- [x] `Image` block renders inline at a capped size.
- [x] `ReadImage` tool, or an image-aware branch in `ReadTool`.
- [x] Pruner tier one elides image parts before tool bodies.

## Phase 7: the rest of the test layer

- Piece 4, the stub backend, is already done: it landed early, in phase 3, as
  the scripted backend `SubagentRegistry`'s own tests run against. See phase
  3's checklist item above. A later reader picking up this phase should not
  build it again.
- [x] Fake `claude` binary replaying a fixture, pointed at by an env override.
- [x] Process lifecycle tests: turn boundary, interrupt, respawn, voice restart.
- [x] `cargo-llvm-cov` line coverage, to find uncovered modules.
- [ ] `cargo-mutants` on `agent/`, `api/`, and `backend/`. Deferred: a full
      run was estimated at 5.8+ hours on this machine, likely an undercount.
      See `docs/notes/mutants.md` for the measured evidence and a script for
      a future run on better hardware.

## Unscheduled

- [x] Extract the settings sidebar out of `src/gui/mod.rs`. It now lives in
      `src/gui/settings_panel.rs`, and `mod.rs` fell from 4534 lines to 3986.
