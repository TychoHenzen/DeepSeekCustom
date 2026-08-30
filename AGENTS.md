# DeepSeekCustom agent guidance

## Project

DeepSeekCustom is an experimental Rust harness for AI coding agents. It provides four backend kinds behind one loopback web application:

- In-process API backends for DeepSeek and Ollama.
- A Claude CLI backend using a `claude -p` child process.
- A Codex CLI backend using `codex exec --json`.
- A test-only stub backend.

The workspace uses Rust edition 2024.

Read [docs/agent-project-context.md](docs/agent-project-context.md) when a task needs the detailed architecture, implementation history, or subsystem map. Do not load that full document for a small isolated change.

## Workspace layout

| Path | Responsibility |
|---|---|
| `crates/deepseek-custom` | Production library, binary, and voice examples |
| `crates/deepseek-custom-tests` | Integration tests, fake CLI binaries, probes, and fixtures |
| `docs` | Maintained design and operational documentation |
| `settings.json` | Local harness configuration |

The root `Cargo.toml` is a workspace manifest. Production source lives under `crates/deepseek-custom/src`.

Tests live under `crates/deepseek-custom-tests/tests/it`. They form one integration-test target through `tests/it/main.rs`.

## Commands

Use these commands from the repository root:

```powershell
cargo check --workspace
cargo build
cargo build -p deepseek-custom
cargo build --release
cargo test --workspace
cargo test -p deepseek-custom-tests
cargo test --workspace -- --test-threads=1
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```

Run a focused test with the integration target before its filter:

```powershell
cargo test -p deepseek-custom-tests --test it skills
cargo test -p deepseek-custom-tests --test it procedure_sandbox_e2e -- --test-threads=1
```

Do not use `cargo test --lib` as the project test command. The production crate intentionally carries no library tests.

## Structural invariants

### Production and test separation

Keep test code in `crates/deepseek-custom-tests`. Do not add inline test modules to the production crate.

The `test-support` feature exposes only seams required by the external test crate. Keep it disabled by default.

Use `cfg(feature = "test-support")`. Do not restore `cfg(any(test, feature = "test-support"))`.

Prefer an existing public seam before adding a new test-only wrapper. A real public type, free function, alias, or constant should remain ordinary public API when tests are not its only consumer.

### Backend boundary

`Backend` has `Api`, `ClaudeCli`, `CodexCli`, and test-only `Stub` variants. `BackendFactory` owns construction.

The Claude CLI and Codex CLI drivers are separate adapters. Do not force one runtime's event, permission, tool, or session assumptions into the other.

The Codex driver launches one-shot JSONL processes and resumes with its stored thread id. Codex owns tools, instructions, context, and permissions inside that child process.

The Claude driver follows Claude's stream and session formats. Keep Claude-only fields inside that adapter.

### Agent sessions

`Task`, `SendMessage`, and `CloseSession` form one lifecycle. A session left open by `Task` must be reused or closed.

When resetting or ending a parent conversation, close every child session it still owns.

Bound dispatch depth and open sessions. Do not assume a completed child response closed its session.

### Windows process spawning

Resolve commands through `PATH` and `PATHEXT` before spawning on Windows. Run `.cmd` and `.bat` files through `cmd /c`.

Do not replace this with a direct `Command::new("npx")` call. Native Windows cannot execute `npx.cmd` as a binary.

### Configuration compatibility

The harness intentionally reads several Claude-compatible file formats. A reference to `CLAUDE.md`, `MEMORY.md`, `settings.json`, or Claude-style tool names may describe product behavior rather than an instruction for the current coding agent.

Trace the runtime path before changing a format for portability.

Do not place credentials, device-specific absolute paths, or local account data into committed configuration.

## Change discipline

Preserve unrelated working-tree changes. Do not stash, reset, overwrite, or include them without explicit scope.

For a bug, reproduce the failing behavior before changing production code when practical.

Run the focused check after each coherent change. Run `cargo test --workspace` before claiming repository-wide success.

Run `cargo fmt --all -- --check` and `cargo clippy --workspace -- -D warnings` for changes that affect Rust source.

Do not update recorded test counts by assumption. Use current command output.

## Documentation

Keep this file short enough for Codex instruction discovery. Put architecture narratives, subsystem inventories, and historical evidence in `docs/agent-project-context.md` or a narrower document.

`AGENTS.md` is the canonical shared instruction file. `CLAUDE.md` imports it and contains only Claude-specific additions.
