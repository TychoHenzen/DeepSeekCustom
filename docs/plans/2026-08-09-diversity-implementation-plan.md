# Implementing Diversity.md, 2026-08-09

`docs/Diversity.md` is a research summary, not a spec. This plan turns its
eleven recommendations into work items. It maps each one to the real
architecture in `CLAUDE.md`. It says plainly where a recommendation is
already done, already covered by an existing MCP server, or not worth
building here.

Everything below is `Api`-only unless stated otherwise. The `claude_cli`
path already hands reasoning, tool choice, and context management to Claude
Code itself. None of this harness's own machinery runs on that path today.
This plan does not change that.

## Where each recommendation lands

| # | Recommendation | Disposition |
|---|---|---|
| 1 | Force tool use for arithmetic, counting, date math | Build: prompt addition, Phase A |
| 2 | Executable checks over LLM judgment | Build: a check command on the new Cascade tool, Phase B. The general case already exists, see below |
| 3 | Decouple reasoning from formatting | Already true, see below |
| 4 | Active context management | Already done (pruning, theme 1 of the long-term roadmap). One addition, Phase A |
| 5 | Cheap-generate, strong-select | Build: Cascade tool, Phase B |
| 6 | Break work into small steps, vote, drop bad answers before voting | Build: the Cascade tool's voting rule, Phase B |
| 7 | Island models, MAP-Elites search | Build: a programmatic archive and selection module, Phase E |
| 8 | Route work and watch how often it escalates | Build: an escalation path and counters, Phase C |
| 9 | Readability and prose checks | Build: a grade-based gate, Phase D |
| 10 | Critique and revise for style | Build: same phase, D |
| 11 | Plain-language prompt nudge | Build: prompt addition, Phase A |

### Already true, no new code

**#3, keeping reasoning apart from formatting.** This harness never forces a
grammar around a reply. A DeepSeek or Ollama turn streams free text. A tool
call is a separate structured field the model sends beside it, not a
wrapper around the reply itself. There is no JSON-mode toggle to add. None
should be added. The paper's finding argues against building one. It does
not point at a gap.

**#4, active context management, mostly.** Three of the four levers in
Diversity.md already exist. `AgentLoop::maybe_prune_context` runs a two-tier
prune with hysteresis (see "Context pruning" in `CLAUDE.md`). Tool-result
elision is tier one of that pruner. `Task` and the new Cascade tool are
already the sub-agent isolation lever. One piece is missing: a compaction
summary. Diversity.md notes that Anthropic pairs compaction with a progress
file the agent re-reads. This harness prunes by dropping content, not by
summarizing it. Phase A adds a smaller, lower-risk piece of that gap: one
note per autopilot step, not a general compaction summarizer. See Phase A
for why the bigger version waits.

**#2's general form.** "Add executable checks wherever a task has a
checkable answer" already happens here. `cargo test`, `cargo clippy`, and
the `dod-guard`/`quality-guard` MCP tools already check this repository's
own changes. Diversity.md adds one thing on top. Here is the real gap: a
way to check a dispatched sub-agent's own candidate answer before it gets a
vote. That is the check command on the Cascade tool, Phase B.

### A correction on #7

An earlier draft of this plan pointed #7 at the `evomcp` MCP server instead
of building it here. That was wrong, and worth stating plainly rather than
quietly fixing. `evomcp`'s `solve` and `evolve` tools run cheap generation
scored by a check command, the same shape as this plan's own Cascade tool.
That is fanout and selection, not evolution. Real evolutionary search needs
three things. A population that carries forward across rounds. A fitness
archive. A selection rule that decides which candidates breed next, and
which islands get reset. None of that can live inside an MCP tool call that
an LLM decides to invoke or not. It has to be code this harness owns. It
runs on a fixed schedule. No model sits in the loop for the archive or
selection steps. Phase E below builds that directly.

## Phase A: reliability-floor prompt work

No new modules. Two additions to
`crates/deepseek-custom/src/agent/prompt.rs`. They follow the existing
pattern of `voice_mode_instructions()`. Each is a function returning a
system-prompt block. Each gets appended unconditionally, not gated behind a
flag, since neither is optional the way voice mode is.

1. **Tool-first arithmetic.** A short instruction: run real calculations
   through `Bash` (a one-line Python or shell command), not from memory. Do
   this for exact counts, date math, and any sum that costs something if it
   comes out wrong. Skip it only for the simplest mental math. This restates
   the PAL/PoT finding from Diversity.md section 2 as an instruction, not a
   forced tool call. Forcing a tool call on every number would misfire on
   trivial cases and add needless delay.
2. **Autopilot compaction note.** `run_repeat` in
   `crates/deepseek-custom/src/agent/repeat.rs` already resets history each
   step (see "Autopilot" in `CLAUDE.md`). Add one line to the existing log
   at `<project_root>/.autopilot/decisions.log` at the end of each step: the
   step's task text and its final reply, one line, newlines flattened the
   same way `PolicyStore::append_decision` already flattens them.
   `PolicyStore::recent_decisions` already reads the newest 20 lines back
   into the next answerer prompt. A full compaction summarizer for a long
   running session raises its own design questions: what triggers it, what
   it can drop. That is not part of this plan.

Done when: a fresh chat asked "what's 47 times 83 plus the file count in
this directory" reaches for Bash rather than guessing from text generation.
A fresh autopilot run's decision log gains one line per step.

## Phase B: the Cascade tool

**The gap.** Diversity.md's best-evidenced idea, cheap-model fanout with a
strong model or a check command picking the winner, has no surface today.
`Task` already lets the model dispatch one sub-agent onto a named backend
(see "Task tool" in `CLAUDE.md`). What is missing: dispatching several
attempts at the same prompt and picking a winner by rule, not by hand.

**Why a new tool, not a `Task` option.** `Task` returns one sub-agent's raw
answer. Cascade returns a decision: a winning answer plus the vote count
behind it. Folding that into `Task`'s schema would force every existing
`Task` caller to reason about a voting mode it never uses. A second, smaller
tool is the cleaner cut.

**Schema**, `crates/deepseek-custom/src/tools/cascade.rs`. Registered and
depth-gated by `may_dispatch`, exactly like `Task`:

- `prompt` (required): the shared task text, sent to every attempt.
- `backend` (required): the generation backend. Meant to be the cheap one.
- `n` (optional, default 5): how many attempts to run.
- `vote_k` (optional, default 1): the lead the top answer needs over the
  runner-up to win without escalating. Diversity.md calls this first-to-
  ahead-by-k. Here it runs against a finished batch, not a live stream: `n`
  attempts run at once, and `vote_k` is checked once they are all in.
  Streaming the count as attempts finish is a real option. It adds a second
  code path for a small latency win at low `n`. Left for later, if `n` grows
  large enough to matter.
- `check_cmd` (optional): a shell command run once per candidate, through
  the same path `BashTool` already uses, against the current `working_dir`.
  A candidate whose command exits non-zero gets dropped before voting. This
  is Diversity.md's red-flag step. An exit code is the cheapest red flag
  there is, cheaper than a second model call to judge the output.
- `diversity_hints` (optional, list of strings): one appended per attempt,
  repeating in order once the list runs out. Defaults to a small built-in
  set: "use a different approach or library than the obvious first choice",
  "favor simplicity over speed", "handle edge cases and error paths first",
  "write the plain, direct version". Diversity.md ranks an explicit
  different-approach constraint as the cheapest, strongest diversity lever.
  It is on by default here, not opt-in.
- `escalate_backend` (optional): a stronger backend name, used when no
  candidate reaches `vote_k`. See Phase C.
- `effort` (optional): same meaning and default as `Task`'s own field.

**Voting.** By default, candidates are grouped by exact match on trimmed
text. That is deliberately simple. Diversity.md's own warning is that a
check is only cheap when it is executable or checkable against a known
answer. String matching on open prose will only ever group truly identical
replies. `check_cmd` is the real path for anything with more than one
correct phrasing. Filter to the candidates that pass it, then either return
the first one or still vote among the passers if more than one phrasing can
be right. Both work: when `check_cmd` is set, voting runs only over
candidates that passed.

**Running it.** Reuses `run_subagent` from
`crates/deepseek-custom/src/backend/subagent.rs` with no change to it. Each
attempt is an ordinary `keep_open: false` dispatch. All of them run at once
through `futures::future::join_all` (confirm it is already a workspace
dependency before adding one). Each attempt gets its own `SubagentId` and
sends events through the existing `RoutedEvent` path. Each attempt renders
as its own `Subagent` block under the calling turn. No new block kind is
needed. The tool result names which attempt won, so a user can still find
the losing attempts in the transcript.

**Errors.** One attempt failing does not fail the whole call. That attempt
is dropped from the vote, matching how `Task` already turns a failed
sub-agent into a tool error, not a hard `Err`. Every attempt failing, or
`check_cmd` failing every candidate, comes back as a tool error naming the
backend and attempt count. It never panics.

Done when: a Cascade call against a scripted `StubBackend` (the existing
`test-support` scaffold, see "Multi-turn subagent sessions" in `CLAUDE.md`)
with 5 candidates and a `check_cmd` that rejects three of them returns the
right winner from the other two. A call where every candidate disagrees, and
none reaches `vote_k`, returns the error Phase C turns into an escalation.

## Phase C: escalation and the router that already exists

**The router is the calling model, not new code.** Diversity.md's RouteLLM
and FrugalGPT ideas assume a router is missing. This harness already has
one. Whichever model reads the system prompt decides, per call, whether to
answer directly, dispatch one `Task`, or fan out a Cascade. That choice
already reads the backends listed in `settings.json`, under the plan-
orchestrate-iterate framing already written up in "Task tool (subagent
dispatch)" in `CLAUDE.md`. A second, separate router on top of that would
compete with it, not help it. Two things are still missing, and Diversity.md
names both as necessary once a cascade exists: a path for the case a
cascade cannot settle on its own, and a way to see how often that happens.

**Escalation path.** When a Cascade call's vote does not reach `vote_k`
(this covers the all-rejected case too), and `escalate_backend` was given,
the tool makes one more call. It sends `escalate_backend` the original
prompt, plus every candidate answer and why it was cut (failed `check_cmd`,
or lost the vote). It asks that backend to pick the best one or write its
own. That answer becomes the tool's return value, marked as escalated in
the returned text. Without `escalate_backend`, the no-consensus case stays a
tool error, same as Phase B.

**Escalation counters.** `SharedFlags`
(`crates/deepseek-custom/src/backend/mod.rs`) gains two counters,
`cascade_total` and `cascade_escalated`, both `Arc<AtomicUsize>`. They follow
the same pattern the prompt-cache hit and miss counters already use in the
status bar. Every Cascade call bumps the first, resolved or not. A call that
escalates bumps the second too. The status bar gains one more line, showing
the escalated share once `cascade_total` is above zero. This is a plain
readout, not an alert. Diversity.md's cited failure was a silent spike in
this same rate going unnoticed by anyone. Making the number visible where
the user already looks fixes that. It does not add a limit that could stall
a real run.

Done when: a Cascade call against a `StubBackend` scripted to disagree on
every attempt, with `escalate_backend` set, returns that backend's answer.
The status bar's percentage updates right after the call.

## Phase D: plain-language gate

**The gap.** Nothing here checks a reply's reading level or its prose today.
`filter_for_speech` (`crates/deepseek-custom/src/voice/mod.rs`) strips
markdown and caps length for spoken output. The voice-mode prompt suffix
asks for short, plain replies. Neither one measures anything. Diversity.md's
own finding is that the prompt-only version of this, item 11, only nudges
the model in the right direction. The harness needs a gate that can reject
and retry for real.

**The metric.** A new module,
`crates/deepseek-custom/src/style/mod.rs` (`Api`-only, alongside
`pruning.rs` and `hooks/`). One pure function,
`flesch_kincaid_grade(text: &str) -> f32`, written by hand rather than as a
new dependency. Split into sentences on `.!?`. Split into words on
whitespace. Count syllables per word with a small vowel-group rule. This is
the same rough method every readability tool uses. Diversity.md's own
warning, that these metrics are shallow, applies here too. The gate is a
coarse filter, not a real quality check. It makes no network call and no
model call. It runs cheap on every reply and is easy to unit test against
sentences with a known grade.

**Settings**, a new `style` block in `settings.json`, following the
`voice` block's own shape:

| Field | Default |
|---|---|
| `plain_language_enabled` | `false` |
| `target_grade` | `8.0` |
| `grade_tolerance` | `2.0` |
| `max_revise_attempts` | `2` |
| `critic_backend` | none (falls back to the replying backend itself) |

**The gate.** In `AgentLoop::run`, right before the final `TurnEnd` fires
for a turn (the same point `ConversationSnapshot` is already sent): when
`plain_language_enabled` is on, and the reply passes some minimum length
(short replies skip this, where a grade score is just noise), compute the
grade. If it is over `target_grade` plus `grade_tolerance`, run a critique-
and-revise loop, capped at `max_revise_attempts`. Send the reply plus a
plain rubric ("rewrite in plain language: short sentences, common words,
active voice, no padding") to `critic_backend`. Recompute the grade on the
result. Stop early once it is within tolerance, or once the attempt cap is
hit. The final text, revised or not, is what reaches the transcript and
`TurnEnd`. A `Notice` block, severity `Info`, records that a reply was
revised and how many tries it took. The rewrite stays visible, not silent.

This matches Diversity.md's own warning about self-bias. `critic_backend`
falling back to the replying backend is a real limit, not hidden here. The
setting exists so a user can point it at a different backend once more than
one is configured (a DeepSeek reply checked by Ollama, or the other way
around). Cross-family checking needs no new machinery. The existing
`backends` map and `BackendFactory::build` already cover it.

**What is not built.** Vale, proselint, and the other prose linters
Diversity.md lists catch hedges and weak words a grade formula cannot see.
Wiring an outside Go binary (Vale) into a Windows-native Rust GUI carries a
real platform cost (see "Platform" in `CLAUDE.md` on the batch-file and
quoting issues this codebase already works around). The grade gate plus the
revise loop already give a real rejection step. Vale is a candidate for
later, if the grade-only gate proves too rough in practice. It is not part
of this plan.

Done when: a reply scripted to run long and heavy with jargon, checked
against a `StubBackend` critic, gets revised to a lower grade within the
attempt cap. A reply already under the target passes through untouched,
with no extra model call. The setting defaults to off, so no existing
session's output changes until a user turns it on.

## Phase E: programmatic evolutionary search

**The gap, restated.** Cascade picks a winner once, from one round of
attempts. Real evolutionary search, the island models and MAP-Elites
Diversity.md describes, carries a population forward across many rounds. It
needs a fitness archive and a selection rule. Neither can be a judgment call
made by an LLM mid-conversation. Both have to be fixed Rust code, run the
same way every time on the same inputs. That is the whole point of the
correction above.

**The split.** Two pieces, kept apart on purpose. A pure archive and
selection module holds every decision that must never depend on a model.
That covers which candidates survive, which parent breeds next, and when
an island resets. A thin dispatch layer around `run_subagent` handles the
one piece that must come from a model: writing each new candidate's text.
The dispatch layer calls the archive module. The archive module never
calls a model.

**The archive**, `crates/deepseek-custom/src/evolution/mod.rs` (`Api`-only,
alongside `style/`). Plain data and pure functions, no tool-trait code here:

- `Candidate { text: String, fitness: f64, features: Vec<f64> }`.
- `MapElitesArchive`: a map from a discretized grid cell to the best
  `Candidate` seen for that cell. Insert compares fitness against whatever
  already occupies the cell and keeps the higher one. Discretization is
  fixed-width buckets over each feature dimension, the simplest rule that
  works and the one to revisit if it proves too coarse.
- `Island { archive: MapElitesArchive }`, one or more, run side by side.
  Without a feature vector, an island falls back to plain top-`k` fitness
  elitism instead of a grid. Diversity.md's own account of MAP-Elites still
  applies: keep the best per cell, not just the best overall.
- `select_parent(&Island) -> &Candidate`: deterministic. Round-robins
  through occupied cells rather than always returning the single best, so a
  weaker-but-different cell still gets a turn to breed. This is the
  mechanism that keeps a population from collapsing onto one answer, the
  exact failure mode Diversity.md documents for plain resampling.
- `migrate(&mut [Island])`: run every `migration_interval` rounds. Ranks
  islands by their best candidate's fitness. Resets the bottom half: clears
  their archive, reseeds with the single best candidate found across every
  island. This is FunSearch's own rule, reimplemented here in Rust instead
  of left to a tool call.

Every one of these functions takes and returns plain values. None of them
touches the network, a child process, or a model. Each gets a direct unit
test with hand-built fitness numbers, no `StubBackend` required.

**The dispatch layer**, `crates/deepseek-custom/src/tools/evolve.rs`. New
tool, `Evolve`, registered and depth-gated by `may_dispatch` like `Task` and
Cascade. Schema:

- `prompt` (required): the seed task.
- `backend` (required): the generation backend.
- `generations` (optional, default 10) and `population` (optional, default
  6): round count and candidates per round.
- `fitness_cmd` (required): a shell command, run against `working_dir` the
  same way Cascade's `check_cmd` runs. It receives a candidate's text and
  must print one number to standard output. That number is the fitness.
  This field is required. Without a fitness signal, the archive has
  nothing to select on.
- `feature_cmd` (optional): a shell command that prints a comma-separated
  list of numbers. These describe where a candidate sits in behavior
  space, for example a strategy id or a rough complexity score. Present,
  this drives the `MapElitesArchive` grid. Absent, an island falls back to
  plain top-`k` elitism.
- `islands` (optional, default 1) and `migration_interval` (optional,
  default 5): island count and how often `migrate` runs.
- `mutation_hints` (optional, list of strings): round-robined into each new
  candidate's prompt, the same pattern as Cascade's `diversity_hints`.
  Here each hint is framed as an edit against the chosen parent, not a
  fresh-start instruction.
- `effort` (optional): same meaning as `Task`'s own field.

**One round.** For each island, `select_parent` picks a candidate. The tool
builds a prompt from the seed task, the parent's text, and the next
mutation hint. It dispatches that prompt through `run_subagent`, exactly as
Cascade dispatches each of its attempts. The resulting text is scored by
running `fitness_cmd`, and, if set, `feature_cmd`, against it. The scored
`Candidate` goes into that island's archive through the plain `insert`
comparison above. Every one of those steps after the dispatch call is pure
Rust. The model never sees the archive, never sees another island's
population, and never picks a winner.

**Errors.** A failed generation dispatch drops just that one candidate for
the round. That matches how a failed Cascade attempt is handled. It never
stops the whole run. A `fitness_cmd` or `feature_cmd` that fails to print a
number is a tool error, naming the command and its output. A silent
zero-fitness value would corrupt the archive without saying so.

**Result.** The tool returns the single best candidate across every island
once `generations` completes, its fitness, and its feature coordinates if
any. The full archive is available in the tool result text as a small
table, so a curious user can see the runner-up cells too.

Done when: a synthetic `fitness_cmd` and `feature_cmd` drive `select_parent`
and `migrate` through several rounds. Both are plain scripts with no model
behind them, run against a `StubBackend` generator. The archive's best
candidate at the end is the one those synthetic scripts actually pick as
best. It is not just the one generated first, or generated most often.

## Build order and dependencies

Phase A depends on nothing and can land first. Phase B depends on nothing
but is the largest single piece. Land it before C, since C extends its
schema. Phase D depends on nothing above it. It could move alongside B and
C if useful. It is ranked below them because Diversity.md itself gives it
the weakest evidence. Readability metrics are called shallow in the source
material. The revise loop's gains are about style too, not correctness.
Phase E's archive and selection module, the pure Rust half, depends on
nothing and could be written and tested before Phase B even lands. Its
dispatch layer reuses `run_subagent` the same way Cascade does, so building
Phase E's dispatch half is easiest once Phase B has proven that reuse works.

## Tests

Each phase's "done when" line above names its own proof. Three new test
files join the existing suite in `crates/deepseek-custom-tests/tests/it/`,
following the naming rule in `CLAUDE.md`. `tools_cascade.rs` covers Phase B
and Phase C together, since the escalation path only extends the same test
target. `style.rs` covers Phase D's pure `flesch_kincaid_grade` function,
plus the revise-loop test against `StubBackend`. `evolution.rs` covers
Phase E's archive and selection functions. Those need no `StubBackend` at
all, since they take plain numbers in and return plain values out. Phase
E's dispatch layer gets its own coverage in `tools_evolve.rs`. All of these
follow the module-path-to-filename rule already written up. The ones that
do need a backend use the existing `test-support` feature's
`StubBackend`/`StubTurn` scaffold, not a new mock.

## What this plan does not cover

- Diversity.md's Stage 1 items 2 and 3 (general executable checks, and
  keeping reasoning apart from formatting) are already true. Neither gets
  new code, as stated above.
- A learned router (RouteLLM-style) is not built. The calling model is the
  router. Phase C only adds the escalation path and the visibility a
  router-free design still needs.
- Vale and proselint integration is named as a possible follow-up in
  Phase D. It is not committed to.
- Phase E's feature-space grid uses fixed-width buckets. That is a known
  simplification, not something Diversity.md itself flags. Revisit it if
  real use shows candidates piling into a few cells instead of spreading
  out.
