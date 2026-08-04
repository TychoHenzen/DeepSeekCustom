# Long-term roadmap, 2026-08-04

Seven themes of future work, in the order they should be built. The order is
not a preference. Each theme depends on the one before it.

1. Structured transcript, replacing the flat line buffer
2. Subagent blocks in the transcript, collapsible and live
3. Multi-turn subagent sessions
4. A switchable working directory
5. One effort control across every backend
6. Image input
7. A real test layer under all of it

Theme 7 is last only in this list. In practice its first slice lands before
theme 1, see "Test work is not a phase" below.

Themes 5 and 6 are the two that could move. Effort control depends on nothing
above it and could be pulled forward at any point. Image input wants the
structured transcript from theme 1, so it cannot move earlier than that.

## Where the code is today

Facts checked against the tree at commit `cf53b27`.

- The GUI transcript is `output_lines: Vec<(String, Color32)>` in
  `src/gui/mod.rs:53`. A line's color is its type. `render_chat_output`
  reconstructs blocks by scanning for runs of `Color32::WHITE` and feeding
  those to the markdown viewer.
- `handle_stream_event` in `src/gui/mod.rs:436` flattens every `StreamEvent`
  into one or more of those lines.
- `spawn_event_drain` in `src/backend/subagent.rs:155` reads a subagent's
  event channel and throws every event away.
- `run_subagent` takes a prompt and returns one final string. The `claude_cli`
  path runs through `src/backend/claude_cli/one_shot.rs`, a process that exits
  when the answer is done.
- `BashTool`, `ReadTool`, and `WriteTool` are each constructed with
  `project_root` (`src/backend/factory.rs:203`). `SystemPrompt` separately
  reads `std::env::current_dir()` (`src/agent/prompt.rs:15`). Nothing can
  change either after startup.
- `cargo test --lib` passes 471 tests. `tests/` holds three fixture files and
  no test target. There is no HTTP mock, no stub backend, and no fake `claude`
  binary.

## Phase 1: a structured transcript

**Why first.** A list of colored strings cannot express a message boundary, a
nested block, or a value that changes after it was appended. Chat bubbles need
the first, subagent blocks need the second, and streaming a subagent's output
into an already-drawn block needs the third. Building any of themes 2 through
4 on the line buffer means encoding structure back into text and parsing it
out at paint time. That is the wrong shape, and it gets more expensive with
every feature added on top.

**The model.** A new `src/gui/transcript.rs` holding plain data, no egui types:

```rust
pub struct Transcript { blocks: Vec<Block> }

pub struct Block {
    id: BlockId,
    collapsed: bool,
    kind: BlockKind,
}

pub enum BlockKind {
    User { text: String },
    Assistant { spans: Vec<Span> },      // Text and Reasoning, in order
    ToolCall { tool: String, args: String, output: Option<String>, is_error: bool },
    Subagent { .. },                     // phase 2 fills this in
    Notice { text: String, severity: Severity },
}
```

`BlockId` is a counter. Blocks are appended and then updated in place: a
`ToolCall` block is created on `ToolCallStart` with `output: None` and filled
on `ToolCallEnd`. An `Assistant` block accumulates spans across a turn instead
of starting a new line per delta.

**Rendering.** `render_chat_output` walks blocks and picks a widget per kind.
User and assistant blocks get a bubble: an indented frame with a background
tint and a role label. Assistant text spans still go through
`egui_commonmark`. Reasoning spans render dimmed and collapsed by default.
Tool calls render as a one-line summary that expands to the arguments and
output. The raw-output toggle stays, and renders the same blocks as plain
text.

**What this replaces.** The color-as-type convention goes away. Color becomes
a rendering choice made from `BlockKind`, not the thing that carries meaning.
The tests that assert on line colors (`user_input_line_is_blue_not_white`,
`model_output_is_white_for_markdown_rendering`, and the rest of that group in
`src/gui/mod.rs`) get rewritten to assert on block kinds and spans. Those
assertions get stronger, not weaker: they check structure instead of paint.

**Done when.** Every `StreamEvent` maps to a block mutation. The transcript
model has unit tests that build it from a scripted event sequence, with no
GUI. The chat tab looks like a chat log.

## Phase 2: subagent blocks

**The gap.** A subagent today is invisible. It logs two lines at `info` and
its events are dropped on the floor. From the chat window a `Task` call is a
tool call that hangs for a while and then returns text.

**Event routing.** `StreamEvent` gains no new variants. Instead the channel
carries a routed envelope:

```rust
pub struct RoutedEvent {
    pub route: Vec<SubagentId>,   // empty for the main session
    pub event: StreamEvent,
}
```

`run_subagent` allocates a `SubagentId`, and `spawn_event_drain` is replaced
by a forwarder that pushes each event onto the parent sender with its own id
prepended to the route. Nesting falls out of this for free: a depth-2 subagent
arrives with a two-element route.

The GUI resolves a route to a block by walking the block tree, and appends to
that block's own inner `Transcript`. So a subagent block holds a full
transcript of its own, rendered with the same code as the top level.

**Rendering.** A `Subagent` block is an `egui::CollapsingHeader`, collapsed by
default. The header carries the backend name, the resolved model, the depth,
a state badge (running, done, failed, interrupted), and elapsed time. Open it
and the subagent's own transcript renders inside, indented. Its tool calls and
its own nested subagents are all in there.

**Open question to settle before building.** Whether a running subagent's
block auto-expands. Suggested answer: collapsed by default, with a live
one-line status in the header. A fanout of five subagents must not push the
main conversation off screen. The user can pin one open.

**Done when.** A `Task` dispatch produces a collapsible block that fills in
live, and a nested dispatch renders nested. The main transcript stays readable
with three subagents running.

## Phase 3: multi-turn subagent sessions

**The gap.** `run_subagent` is one shot by construction. The prompt goes in,
the final text comes out, the backend is dropped. A caller that wants to ask a
follow-up has to re-dispatch with the whole context restated in the prompt.

**The shape.** A `SubagentRegistry`, owned alongside `BackendFactory`, holding
live sessions keyed by `SubagentId`. Each session owns its `Backend` and its
own history. Three tool surfaces instead of one:

- `Task` keeps its current schema and gains an optional `keep_open` flag. It
  returns the final text plus the session id when the session stays open.
- `SendMessage { session_id, prompt }` sends another turn into an open session
  and returns that turn's final text. The session keeps its history, so the
  prompt can be a short follow-up.
- `CloseSession { session_id }` ends it. Optional, since the lifetime rule
  below closes sessions anyway.

**Lifetime.** A session lives until its parent's turn ends, or until it is
closed, whichever comes first. Any other rule leaks child processes and API
history across a session reset. `Reset` closes every session. An idle timeout
is a second safety net worth adding, not a substitute.

**The claude path changes.** A multi-turn `claude_cli` subagent cannot use
`one_shot.rs`. That process exits after one answer. It needs the long-lived
driver in `process.rs`, one instance per session. That code already handles a
turn boundary over `turn_done`, so this is a reuse, not a rewrite.
`one_shot.rs` stays for the `keep_open: false` case, where spawning a
long-lived child would buy nothing.

**Depth still applies.** `may_dispatch` gates `SendMessage` exactly as it
gates `Task`. A subagent at the depth limit gets neither tool.

**Risk to name plainly.** This is the theme most likely to produce a runaway
cost. Two open sessions talking to each other is a loop with no natural
stopping point. Mitigations: a per-session turn cap, a per-parent-turn cap on
total `SendMessage` calls, and both counts visible in the subagent block
header. Build the caps in the same change as the feature, not after.

**Done when.** A parent dispatches a subagent and reads its answer. It then
sends a follow-up that refers to that answer without restating it, and gets a
coherent reply. Both turns render inside one subagent block.

## Phase 4: a switchable working directory

**The gap.** There is no way to change where tools run. There are also two
different notions of "here" in the code that nobody reconciled: the tools use
`project_root`, while the system prompt reports `std::env::current_dir()`.
When the process starts somewhere other than the project root, the model is
told one directory and its tools act on another.

**The split to make explicit.** Two roots, named apart, with different rules:

- `project_root` is the config anchor. `settings.json`, `CLAUDE.md`,
  `MEMORY.md`, `skills/`, `autopilot-policy.md`, and `.autopilot/` all resolve
  against it. It is fixed at startup and never changes. Nothing about a
  working directory change should move where preferences are saved.
- `working_dir` is where tools act. `BashTool` runs commands there, `ReadTool`
  and `WriteTool` resolve relative paths there, and the system prompt reports
  it. It starts equal to `project_root` and can change at runtime.

**Mechanism.** `working_dir` becomes an `Arc<Mutex<PathBuf>>`, shared the way
`model_flag` and `context_budget_flag` already are. The tools read it per
call, not at construction, so a change takes effect on the next tool use.
`SystemPrompt` reads it each turn in `sync_dynamic_config`, alongside the
thinking and voice flags, and stops calling `std::env::current_dir()`
entirely. The process's own cwd is never changed: a global mutated from a tool
call would race against every other task in the runtime.

**The two paths that need more than a shared flag.** The `claude_cli` child
takes its cwd at spawn time. So `ClaudeCliDriver` restarts the child on a
change, the same way it already restarts on a voice-mode change. Skills and
memory load from `project_root` at startup, so a change leaves them alone.
That is the right call. State it in `CLAUDE.md` so a later reader does not
"fix" it.

**Surfaces.** A `Cd { path }` tool. It checks that the target exists and is a
directory, and returns a tool error rather than a hard `Err` when it is not. A
directory field in the settings sidebar with a folder picker. The current
working directory in the status bar next to the backend name. A `working_dir`
override on `Task`, so a subagent can be pointed at a sibling checkout.

**Open question to settle first.** Whether `working_dir` is allowed outside
`project_root`. Suggested answer: yes, with no path sandbox. This harness
already runs Bash with no permission prompt. A sandbox that one tool can
trivially escape is a false promise. Say that out loud in the docs rather than
implying a containment that does not exist.

**Done when.** Changing the directory in the GUI changes three things at once.
A Bash command runs there. A relative `Read` resolves there. The model is told
so. The `claude_cli` backend picks it up on the next turn.

## Phase 5: one effort control across every backend

**The gap.** Three backends express reasoning effort three different ways, and
the GUI exposes one boolean for all of them. DeepSeek takes `thinking_mode`
with the values `thinking`, `thinking_max`, and `non-thinking`. Ollama takes a
top-level `reasoning_effort` with `max`, `high`, `medium`, `low`, and `none`,
and `prepare_request` in `src/api/client.rs` translates the DeepSeek values
into it, collapsing five levels into two. The `claude_cli` path gets nothing
at all: the thinking toggle does not reach it. So `thinking_max` is
unreachable from the UI, Ollama's middle levels are unreachable, and Anthropic
is unreachable entirely.

**The model.** One ordered enum owned by this harness, with a per-provider
mapping at the edge:

```rust
pub enum Effort { None, Low, Medium, High, Max }
```

`prepare_request` maps it per provider, which is exactly the job that function
already does. DeepSeek gets `non-thinking` for `None`, `thinking` for `Low`
through `High`, and `thinking_max` for `Max`, until DeepSeek offers more
levels. Ollama gets its five values one to one. The `claude_cli` path maps
onto whatever the CLI accepts, and that needs checking against the real binary
before the mapping is written, not guessed from memory.

**Surfaces.** The sidebar's thinking checkbox becomes a five-stop slider or a
combo box, seeded from settings and persisted through an `apply_effort`
function like every other control. The shared flag becomes an
`Arc<AtomicU8>`, read each turn in `sync_dynamic_config` the same way
`thinking_flag` is today. `Task` gains an optional `effort` field, so a parent
can dispatch a cheap subagent at `low` and a hard one at `max` in the same
turn. That per-dispatch control is the real payoff here, and it fits the
stated plan-orchestrate-iterate chain better than a global toggle does.

**What this replaces.** `thinking_flag` and the `thinking.enabled` settings
block. Both go away rather than living alongside the new control. A boolean
that means "high or none" is not worth keeping once there are five levels.

**Done when.** Every level reachable from the sidebar produces a different
request on each of the three backends, checked against a mock. A `Task`
dispatch can override it per subagent.

## Phase 6: image input

**The gap.** `Message.content` is `Option<String>` (`src/api/types.rs:34`).
There is no way to attach an image to a turn, and no way to render one back.

**Why it comes after phase 1.** An image in the transcript is a block that
holds bytes, not a line of colored text. The line buffer cannot carry it at
all. Once `BlockKind` exists, an `Image` variant is a small addition, and the
paste and drop handlers write into the same structure everything else does.

**The wire change.** `content` becomes a small enum that still serializes as a
bare string when it holds only text:

```rust
pub enum Content {
    Text(String),
    Parts(Vec<ContentPart>),
}

pub enum ContentPart {
    Text { text: String },
    ImageUrl { url: String },   // data: URL with base64 payload
}
```

The OpenAI-compatible shape both API providers speak takes
`{"type":"image_url","image_url":{"url":"data:image/png;base64,..."}}`. Custom
serialization keeps every existing text-only message on the wire exactly as it
is today, so no request that works now changes shape. That matters: a needless
shape change would have to be revalidated against three backends.

**Per-backend reality, to check before building.** Ollama supports images only
on a vision model. Sending one to `qwen2.5-coder` will fail. DeepSeek's
support needs confirming against the live API rather than assumed. The
`claude_cli` path looks unable to take an image over its stdin turn shape.
That path most likely needs the image written to a temp file and named by path
in the prompt text. Answer each of those with a real request before writing
the mapping. Where a backend cannot take an image, the harness should say so
in the transcript rather than dropping the attachment silently.

**Surfaces.** Ctrl+V pastes an image from the clipboard into the pending turn.
Drag and drop does the same, and eframe already delivers dropped files. A
pending-attachment strip sits above the input box with a thumbnail and a
remove button. The transcript renders an `Image` block inline, at a capped
size, clickable to open full size. A `ReadImage` tool, or an image-aware
branch in `ReadTool`, lets the model pull a screenshot off disk itself. That
last part is what makes this useful for the agent, not just for the human.

**Cost note worth stating.** Images are expensive per turn and they never
prune well: a pruned image is either fully present or fully gone. Tier one of
the pruner should elide image parts before it elides tool bodies, since one
screenshot outweighs a lot of text.

**Done when.** A pasted screenshot reaches a vision-capable backend and the
model answers about it. The image renders in the transcript on both sides of
the turn.

## Phase 7: a real test layer

**Current state, stated honestly.** 471 tests pass and they are not nothing:
parsers, config round-trips, event mapping, and pure helpers are covered well.
What is missing is every test that crosses a boundary. There is no test target
in `tests/` at all. Nothing exercises a full turn. Nothing runs the real
`ApiClient` against anything. Nothing runs `run_subagent` end to end. Nothing
starts a `claude_cli` child. The GUI tests all call free functions, because
the paint path was never testable.

That is why "tests are lacking" and "471 tests pass" are both true. The suite
covers the parts that were easy to isolate. It skips the parts where the bugs
have actually been. Three examples: the `input_json_delta` buffering bug, the
interrupt flag that did not survive an iteration, and the scoring model that
was wrong per provider. Every one of those lived at a boundary.

**Five pieces, roughly in this order.**

1. **An HTTP mock for the API path.** Add `wiremock` as a dev dependency.
   `BackendConfig` already carries an optional `base_url`, so a test can point
   a real `ApiClient` at a local mock with no production change. Five first
   tests. A full turn with a canned SSE stream. A turn that calls a tool and
   feeds the result back. A retry after a 500. A broken chunk. A stream that
   ends without `[DONE]`.
2. **A fake `claude` binary.** A small Rust test helper that replays
   `tests/fixtures/claude_stream_json.jsonl` on stdout and reads stdin,
   pointed at by the spawn path through an env override. That makes the
   process lifecycle testable: a turn boundary, an interrupt killing the
   child, a respawn after the child exits, and a voice-mode restart.
3. **Transcript tests.** Cheap once phase 1 lands, because the transcript is
   plain data. Build one from a scripted event sequence and assert on the
   block tree. This is where subagent routing gets covered without a GUI, and
   later where an image block is checked without a window.
4. **A stub backend for subagent tests.** A `Backend` variant or a trait
   implementation that answers from a script. It makes `run_subagent`, the
   depth limit, the registry lifetime rules, and the phase 3 turn caps all
   testable without a network call or a child process.
5. **Making the existing suite honest.** The voice tests need real model files
   on disk and silently depend on them. Move them behind a cargo feature such
   as `voice-models`. Then a plain `cargo test` on a fresh checkout is green,
   and the skip is explicit rather than a mystery failure.

**Two measurements worth adding once the above exists.** `cargo-llvm-cov` for
line coverage, to find whole modules with no boundary coverage rather than to
chase a percentage. `cargo-mutants` on the core modules, `agent/`, `api/`, and
`backend/`, to answer the harder question of whether the tests would actually
fail if the code were wrong.

**Done when.** `tests/` holds integration targets that exercise a full turn on
both backend kinds, with no network call. A deliberate one-line break in
`agent_loop.rs` or `map.rs` makes something red.

## Test work is not a phase

Piece 1 of phase 7, the HTTP mock, should land before phase 1 starts. Phases 1
through 6 all rewrite code that has no boundary test today. Doing that blind
is how a regression gets shipped. The rest of phase 7 spreads across the other
six. Transcript tests belong in phase 1's own change. The stub backend belongs
in phase 3's. The effort mapping table is a mock test written with phase 5.
The content round-trip test is written with phase 6. Each phase's "done when"
line already assumes a test proves it.

Phase 7 is listed separately for one reason. The measurement work and the
honesty work on the existing suite are their own tasks. Testing does not come
after building.

## Smaller items, unscheduled

- `settings.json` in the repo sets `default_backend` to `ollama`, while
  `CLAUDE.md` says it ships as `deepseek`. One of the two is wrong. Fix the
  doc or the file, and check the same claim in `AGENTS.md`.
- `src/gui/mod.rs` is 2467 lines. Phase 1 splits the transcript out of it
  anyway. The settings sidebar is the next obvious extraction.
- `ContextPruner` is gone from `src/context/mod.rs`, which now holds only
  `ThinkingStore` and `parse_thinking_tags`, both unused by the agent. Either
  wire them in or delete them.

## What is deliberately not here

- Phase 3 of the original plan, the hemisphere model. It is unrelated to
  everything above and the stub in `src/hemisphere/` can wait.
- Keeping a conversation on disk between runs. This note said to revisit after
  phase 1, when there was a real model to persist. Phase 1 landed, and that
  revisit happened: this is now built. `src/session/mod.rs` holds the data
  model, `src/session/store.rs` the disk layer, and a third Sessions tab lists
  saved conversations. See `CLAUDE.md`'s "Session persistence" section for the
  full mechanism.
- Permission prompts. This harness runs Bash without asking on purpose, and
  the `claude_cli` path is spawned with `bypassPermissions` for the same
  reason. Adding prompts is a product decision, not a gap.
