# cargo-mutants, deferred

Phase 7's second measurement, "to answer the harder question of whether the
tests would actually fail if the code were wrong," on `agent/`, `api/`, and
`backend/`. This note records why a full run was not attempted on this
machine, the real numbers behind that call, done 2026-08-05, and a script for
a future run on better hardware.

## What mutation testing would tell us that coverage cannot

`docs/notes/coverage.md` answers "which lines never run." It cannot answer
"if this line ran the wrong logic, would a test catch it." A mutant is a
small deliberate bug (delete a `!`, swap `&&` for `||`, replace a function
body with a fixed value) inserted at a real call site. `cargo-mutants`
rebuilds the crate with that one change and reruns the test suite. A test
failure means a mutant "caught." A green run on mutated code is a "missed"
mutant: a line the suite executes but does not actually check. Coverage
cannot see that gap; only a real broken build run through the real tests can.

## Tooling check

```
$ cargo mutants --version
cargo-mutants 27.0.0
```

Already installed on this machine. No install step was needed.

## The real mutant count

```
cargo mutants --list -f 'src/agent/*.rs' -f 'src/api/*.rs' -f 'src/backend/*.rs' -f 'src/backend/**/*.rs'
```

`--list` only parses the source for mutation sites; it builds nothing and
runs no tests. This ran in a few seconds and printed real output:

```
600
```

That is the line count of the list file, one mutant per line, counted with
`wc -l`. 600 real mutants across those three directories, not an estimate.

## The measured per-mutant build cost

Two real, timed builds, both after a normal `cargo build -j 1` had already
finished (baseline `cargo build -j 1` from a stale target: 5m 48s, itself a
measured number, not the one being estimated from).

1. Appended one comment line to `src/agent/prompt.rs`, then timed
   `cargo build -j 1`: **36.6s** real time, dependencies (whisper-rs, ort,
   eframe) already built and cached, only this crate recompiling.
2. Reverted that line, appended a different one-line comment, then timed
   `cargo test -j 1 --lib`: **34.0s** real time, including relinking and
   running all 765 lib tests, which alone took 1.13s. Build time dominates;
   test execution is nearly free next to it.

Both probe lines were reverted immediately after each measurement.
`git diff src/agent/prompt.rs` was checked after both reverts and matched
the pre-existing uncommitted diff already in this working tree exactly, with
no leftover probe line.

So one mutant's real build-and-test cost, on this machine, with the crate's
dependencies already warm, is about 34-37 seconds. Call it 35s.

## Corroborating evidence: a real run was already in progress

While measuring the above, `tasklist` showed a live `cargo-mutants.exe`
process (PID 44508) already running on this machine, writing to
`mutants.out/`. This was not started by this task; it is a leftover process,
most likely the one the user describes stopping earlier, that in fact kept
running in the background. Reading its on-disk state is a real observation,
not a run this task initiated or extended.

Its own `mutants.out/outcomes.json`, read at the time of writing this note
(`end_time: null`, so still in progress), scopes to 9 files across
`agent/`, `api/`, and `backend/` (a subset of the 600-mutant scope above,
232 total mutants for that subset per its own `mutants.json`). Its baseline
build (`cargo test --no-run -j1 --lib`) took **503.1s, about 8.4 minutes**,
in its own recorded `phase_results`, close in scale to the coverage note's
"about 10 minutes" for a heavier instrumented build. Of its 232 mutants, 143
had been processed by the time this was read: 68 caught, 55 missed, 2
timeout, 18 unviable. Summing every processed mutant's recorded build and
test phase durations gives **4307.3s across 143 mutants, an average of
30.1s per mutant**, real numbers read from its own output file.

That 30.1s figure, from an actual in-progress run, corroborates the 34-37s
this task measured directly with isolated probes: same order of magnitude,
same machine, same constraints, two independent sources.

## The estimate, and what it does not include

Using the more directly comparable, actually-observed 30.1s/mutant figure
against the real 600-mutant count for the full `agent/`, `api/`, `backend/`
scope: 600 x 30.1s = 18,060s = **about 5.0 hours**, run serially. Using this
task's own isolated 35s probes instead: 600 x 35s = 21,000s = **about 5.8
hours**. Both are arithmetic on measured numbers, not an observed run over
the full scope. Label: **estimate**, bracketed between about 5.0 and 5.8
hours by the two independent measurements above.

It is very likely an undercount, for reasons not measured here:

- `-j 1` for `cargo build`/`cargo test` is required on this machine; the
  default parallel build crashes rustc with `STATUS_STACK_BUFFER_OVERRUN`
  when linking four heavy targets at once (`CLAUDE.md`, `.cargo/config.toml`).
  My 35s probes already ran under `-j 1`. But `cargo-mutants` itself needs
  that flag threaded through to every one of its 600 internal build/test
  invocations, and its own top-level `--jobs` concurrency is a separate
  knob from cargo's build parallelism; getting both right without
  re-triggering the crash needs care a `--list`-only check cannot confirm.
- My probes reused an already-built `target/` in place. `cargo-mutants`
  defaults to copying the source tree into a scratch build directory per
  mutant (`--copy-target` controls whether `target/` is copied too). Copying
  a 950MB `target/` directory (measured with `du -sh target`) 600 times, or
  losing the dependency cache and recompiling whisper-rs/ort/eframe from
  scratch for even a fraction of those 600 runs, would each individually
  cost minutes, not seconds, dwarfing the 5.8-hour estimate.
- Incremental compilation is off on purpose (`.cargo/config.toml`), because
  it ballooned to over 10GB of temp files across a handful of runs. Six
  hundred separate build directories under the same constraint is a real
  disk-usage risk this note has not sized.

None of these three are measured here. They are named because each one
alone could push the real number well past the 5.8-hour arithmetic estimate,
possibly by a large multiple. The honest floor is "at least half a day of
uninterrupted machine time," not "5.8 hours."

## Why this crate is expensive to mutate

- It pulls in whisper-rs, ort, and eframe, which are what made the phase 7
  coverage build (`docs/notes/coverage.md`) take about 10 minutes for one
  instrumented build, not the 35s this note measured for an ordinary
  incremental one.
- `-j 1` is mandatory for every build in this repository, so there is no
  parallelism escape hatch for cutting the per-mutant cost down.
- Incremental compilation is disabled, so any run shape that does not reuse
  a warm `target/` in place pays close to the full build cost every time,
  and `cargo-mutants`'s default scratch-directory copying is exactly that
  run shape.

## Repeatable script for a future run

`scripts/mutants.ps1` runs the same list command this note used, plus the
real filtered run commented out and ready to uncomment on better hardware
(more cores, more disk, or a machine where a full parallel build does not
crash rustc).

## What is still unknown about the suite

Phase 7 closes the boundary-testing gap (the fake `claude` binary, process
lifecycle tests) and the line-coverage gap (`cargo-llvm-cov`). It does not
close the mutation-testing gap. Nobody has checked, on real mutated code,
whether the 765 lib tests plus 20 integration tests would actually fail if
a boundary condition, a sign, or a boolean were wrong at a call site that
line coverage marks as "executed." A line running is not the same claim as
a line being checked. That question stays open until a full `cargo-mutants`
run happens on hardware that can absorb 600 real build-and-test cycles
without threatening the 10GB temp-file and multi-hour costs named above.
