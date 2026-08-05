# cargo-llvm-cov line coverage

Phase 7, "Two measurements worth adding": run `cargo-llvm-cov` to find whole
modules with no boundary coverage, not to chase a total percentage. This note
records one real run against the tree at commit `26b30d0`, done 2026-08-05.

## Tooling check

```
$ cargo llvm-cov --version
cargo-llvm-cov 0.8.7
$ rustup component list --installed
llvm-tools-x86_64-pc-windows-msvc
```

Both were already present on this machine. No install step was needed.

## The run

Repeatable through `scripts/coverage.ps1`, or directly:

```
cargo llvm-cov --summary-only -j 1
```

`-j 1` for the same reason every other test run in this project uses it: the
default parallel build crashes rustc here (`STATUS_STACK_BUFFER_OVERRUN`)
when linking four heavy targets at once, per `CLAUDE.md`.

This ran clean on the first try, no failure, about 10 minutes wall clock
(whisper-rs, ort, and eframe all get instrumented, which is slow). It built
and ran the lib target plus all three integration targets (`api_turn`,
`claude_cli_fake_binary`, `claude_cli_lifecycle`), 765 lib tests plus 20
integration tests, all passing. Totals from the real output:

```
TOTAL   28620   4036   85.90%   2050   234   88.59%   17822   2557   85.65%   0   0   -
```

That is region/function/line coverage columns as `cargo-llvm-cov` prints
them. 85.65% line coverage overall. The number itself is not the point, see
below for what it hides.

An earlier `--lib`-only run (no integration targets) gave TOTAL line coverage
85.76% over 17480 lines, close to the combined number. The difference worth
naming: with the integration targets included, `backend\claude_cli\process.rs`
line coverage rose from 78.82% to 91.93%, and `main.rs` appeared for the first
time at a flat 0.00% (see below). Everything else moved by low single digits
or not at all. Both runs are real; the combined one is the one to trust
because it is the more complete picture.

## What it found, by module

Ordered by line coverage, worst first, for anything below roughly 80%.
Numbers are lines covered / lines missed / percent, from the real table
above, not estimated.

- **`main.rs`, 0/240 lines, 0.00%.** No test target runs the binary's own
  `main()`. Expected: `main.rs` wires up the GUI, the voice service, and
  process startup, none of which a `cargo test` process exercises. Nothing
  here is a gap to close through more unit tests; it would need a real
  end-to-end launch, which is a GUI/gameplay-shaped problem, not a coverage
  one.
- **`gui\sessions_tab.rs`, 35/89 lines, 39.33%.** The Sessions tab's own
  render function and its relative-time formatting are barely touched. The
  existing 5 tests for this file cover the pure relative-time helper; the
  render path itself, same as the rest of the GUI, has no paint-level test.
  `CLAUDE.md` already flags this tab as not yet confirmed on screen by a
  human. This coverage result is the same gap restated in a different form.
- **`voice\stt.rs`, 24/57 lines, 42.11%.** Expected and already named in the
  task briefing: the one test that exercises real transcription sits behind
  the `voice-models` cargo feature, off in this run because the Whisper model
  file is not on this checkout. The `new_reports_missing_model_path_by_name`
  test is the only thing that ran here.
- **`voice\tts.rs`, 146/268 lines, 54.48%.** Same shape as `stt.rs`: the
  synthesis path needs the Kokoro ONNX model and voice pack on disk, and that
  test also sits behind `voice-models`. What ran here are the pure helpers:
  `clamp_speed`, `normalize_for_synth`, the dead-worker draining behavior.
- **`hooks\mod.rs`, 62/115 lines, 53.91%.** Not a missing-model gap like the
  two above. `HookRunner` actually spawning a subprocess and parsing its
  stdout/stderr under real lifecycle events (`PreToolUse`, `PostToolUse`,
  `SessionStart`, `SessionEnd`, `SessionReset`) looks thin next to the module's
  size. Worth a closer look outside this task's scope: is the missing half
  genuinely untestable, or is it an actual boundary gap in the "hooks
  execution integration" work `CLAUDE.md` lists as a "Next" item.
- **`voice\cuda_dlls.rs`, 138/243 lines, 56.79%.** DLL discovery and Windows
  loader registration for CUDA. Its own tests cover the pure path-search
  helpers (`nvidia_bin_dirs_under`, `toolkit_bin_dir_prefers_bin_x64`); the
  actual `LoadLibraryExW`/`AddDllDirectory` calls need a real CUDA install to
  exercise meaningfully and are not going to show real coverage on a machine
  without one wired the same way every run.
- **`voice\capture.rs`, 121/205 lines, 59.02%.** Microphone capture into the
  ring buffer. Needs a real audio device to drive past its buffer-manipulation
  unit tests.
- **`voice\playback.rs`, 144/219 lines, 65.75%.** Same shape: cpal output
  needs a real device; the `wait_for_empty` queue-draining tests are what
  actually ran.
- **`api\client.rs`, 284/424 lines, 66.98%.** This is the one on the list
  that is not a hardware or missing-model story. `tests/api_turn.rs` mocks a
  full turn, a tool round-trip, a retry, a malformed chunk, and a
  no-`[DONE]` stream against `wiremock`, and those five do move the needle
  (66.98% vs 65.57% in the lib-only run). A third of the file is still
  unexercised: `resolve_api_key`'s longer fallback chain
  (`DEEPSEEK_API_KEY` -> `ANTHROPIC_AUTH_TOKEN` -> `settings.json` project ->
  `settings.json` global -> `~/.claude/backends.json`) reads real environment
  and disk state that a wiremock test does not stub, and is a plausible
  reason a chunk of this file stays cold.
- **`gui\mod.rs`, 2181/3026 lines, 72.08%.** The known, already-documented
  gap: the GUI tests here call free functions,
  never the paint path itself (`CLAUDE.md`'s "GUI" section and the roadmap's
  phase 7 intro both say this in so many words). At 4625 regions this is the
  single largest file in the crate, so its 27.92% miss rate is also the
  largest raw number of missed lines in the whole table, larger than every
  voice module combined.
- **`backend\claude_cli\one_shot.rs`, 185/231 lines, 80.09%.** The one-shot
  subagent driver. Above the arbitrary 80% line drawn here, so not detailed
  further, but the miss is worth naming since it sits right at the boundary:
  a subagent's `claude_cli` one-shot run is exactly the kind of cross-process
  boundary phase 7 called out as previously untested. `tests/claude_cli_*.rs`
  cover the long-lived driver in `process.rs`, not this one-shot path.

## What is not a gap, despite a low number

`main.rs` at 0.00% and the hardware-gated voice modules
(`capture.rs`, `playback.rs`, `cuda_dlls.rs`, `stt.rs`, `tts.rs`) are not
missing tests in the sense of "someone forgot." They are boundaries this test
layer already draws on purpose: `main()` needs a real launch, and the
hardware-facing voice code needs a real microphone, speaker, GPU, or model
file, none of which a CI-shaped `cargo test` run should depend on.
`gui\sessions_tab.rs` and `gui\mod.rs` are the same "paint path is not
testable" gap `CLAUDE.md` already states plainly, not a new finding.

The two findings worth acting on, if this list turns into follow-up work,
are `hooks\mod.rs` and `api\client.rs`'s `resolve_api_key` chain: both are
pure-logic boundaries with no hardware dependency, and both look thinner than
their size would predict.

## Repeatability

`scripts/coverage.ps1` runs the exact command above. Re-run it after
`cargo install cargo-llvm-cov` and `rustup component add llvm-tools-preview`
if either is missing; both were already present here.

## `cargo test -j 1`, confirmed still green after this work

```
$ cargo test -j 1
...
test result: ok. 765 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; ...
```

(Integration targets `api_turn`, `claude_cli_fake_binary`, and
`claude_cli_lifecycle` all passed too, matching the coverage run's own test
output above.)

## Not attempted here

`cargo-mutants`, the second measurement phase 7 names, on `agent/`, `api/`,
and `backend/`. Out of scope for this task; a separate step.
