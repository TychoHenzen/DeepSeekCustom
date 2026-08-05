# List and, when ready, run cargo-mutants over agent/, api/, and backend/.
#
# See docs/notes/mutants.md before uncommenting the real run. On this
# machine a full run was estimated at 5.8+ hours of serial build-and-test
# cycles, and that estimate is very likely low: see the note for the
# unmeasured costs (scratch-directory copying, loss of the warm target/
# cache, 10GB+ temp files from disabled incremental compilation).
#
# Needs cargo-mutants:
#   cargo install cargo-mutants

$files = @(
    "src/agent/*.rs",
    "src/api/*.rs",
    "src/backend/*.rs",
    "src/backend/**/*.rs"
)

$fileArgs = $files | ForEach-Object { "-f", $_ }

# Cheap: lists real mutants without building or running anything. Safe to
# run any time.
cargo mutants --list @fileArgs

# Expensive: the real run. CARGO_BUILD_JOBS=1 is mandatory for every cargo
# build/test in this repository (CLAUDE.md, .cargo/config.toml): the default
# parallel build crashes rustc here with STATUS_STACK_BUFFER_OVERRUN. That
# env var is how a build-level -j 1 reaches every one of cargo-mutants' own
# internal cargo invocations, not just the one this script runs directly.
# Uncomment only on a machine that can absorb several hours and the disk
# usage named in docs/notes/mutants.md.
#
# $env:CARGO_BUILD_JOBS = "1"
# cargo mutants @fileArgs
