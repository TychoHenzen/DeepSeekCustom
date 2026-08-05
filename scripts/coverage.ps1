# Run cargo-llvm-cov and print the per-file line coverage table.
#
# Needs cargo-llvm-cov and the llvm-tools rustup component:
#   cargo install cargo-llvm-cov
#   rustup component add llvm-tools-preview
#
# Uses -j 1. The default parallel build crashes rustc on this machine when
# linking four heavy targets at once (STATUS_STACK_BUFFER_OVERRUN). See
# .cargo/config.toml and CLAUDE.md's Build & Test section.
#
# Runs the lib target plus every integration target under tests/, so the
# summary reflects both unit and boundary coverage. Takes several minutes:
# the build instruments whisper-rs, ort, and eframe.

cargo llvm-cov --summary-only -j 1
