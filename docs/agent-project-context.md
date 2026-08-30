# Detailed agent project context

This document describes the current production architecture. Git history and archived OpenSpec changes hold earlier implementation details.

## Project

DeepSeekCustom is a Rust edition 2024 harness for AI coding agents. It serves one responsive TypeScript web application from an ephemeral loopback HTTP origin.

The application supports four backend kinds behind one application actor:

- DeepSeek and Ollama through the in-process API backend.
- Claude through a `claude -p` child process.
- Codex through one-shot `codex exec --json` processes with thread resume.
- A test-only scripted stub.

## Workspace layout

| Path | Responsibility |
|---|---|
| `crates/deepseek-custom` | Production library, binary, web server, and voice examples. |
| `crates/deepseek-custom-tests` | One integration target, fake binaries, browser harnesses, probes, and fixtures. |
| `web` | TypeScript source, Vite build, component tests, and browser scripts. |
| `crates/deepseek-custom/src/web/assets` | Generated production files embedded into the Rust binary. |
| `docs` | Maintained design, setup, testing, and operational notes. |
| `settings.json` | Local runtime configuration. Do not treat its current values as documentation. |

Production code has no inline test modules. The `test-support` feature exposes only seams needed by the external test crate. It stays disabled by default.

## Production startup

`main.rs` loads settings, creates backend and service ports, starts the application actor, and binds the web server to `127.0.0.1` on an operating-system-selected port. It reports the final URL and opens it in the default browser.

The binary serves the shell, hashed JavaScript and CSS, and API routes from the same origin. `rust-embed` supplies the production files. Runtime startup does not invoke Node, npm, or Vite.

`run.ps1` builds the Rust workspace and starts `target/debug/deepseek-custom.exe`. See `docs/web-production-assets.md` for the asset build boundary.

## Application boundary

`crates/deepseek-custom/src/application/` owns visible state and command arbitration. The actor serializes authoritative `AppSnapshot` values and revisioned changes. Browser commands carry an anti-forgery token and an expected revision.

Typed ports isolate backend turns, saved sessions, settings, folder selection, voice, Autopilot, Cascade, Evolve, Procedure, and test execution. Slow work runs behind those ports. The actor remains responsive and rejects stale results by operation identity.

The web server exposes a bootstrap snapshot, a Server-Sent Events stream, typed command endpoints, attachment upload, and a health route. It limits request sizes and concurrent streams. It accepts requests only on the selected loopback origin.

## Frontend

`web/src/` contains the responsive application shell and eight workspaces: Chat, Sessions, Settings, Autopilot, Cascade, Evolve, Procedure, and Tests.

The frontend bootstraps once, reduces ordered revision events, and reconnects after a dropped stream. A revision gap triggers a fresh snapshot. Commands are disabled while state is stale or while an incompatible operation owns the application.

Chat renders ordered user, reasoning, text, tool, notice, error, image, and terminal blocks. Browser paste, drop, and file selection use the attachment upload contract. Session switching waits for an active turn to reach a terminal state.

Settings preserve the fixed `project_root` and mutable `working_dir` boundary. The folder button calls the native folder-picker port. A cancelled dialog leaves the prior directory unchanged.

Voice controls call the Rust voice port. Push-to-talk keyboard handling lives in the focused browser application. Speech capture, transcription, synthesis, and playback remain local Rust services.

Procedure renders running, awaiting-review, approved, rejected, failed, and interrupted states from authoritative application data. Review and apply commands carry the run identity. See `docs/procedure-localization-verification.md` for maintained browser evidence.

## Backend boundary

`Backend` has `Api`, `ClaudeCli`, `CodexCli`, and test-only `Stub` variants. `BackendFactory` owns construction and applies the shared root-session flags.

The API backend runs DeepSeek or Ollama turns through `reqwest`. It merges streamed tool-call fragments, retains reasoning content required by the provider, executes tools through `ToolRegistry`, and observes bounded turn and interrupt controls.

The Claude and Codex CLI drivers are separate adapters. Claude owns its stream and session formats. Codex owns JSONL events, permissions, tools, context, and its stored thread id.

`Task`, `SendMessage`, and `CloseSession` form one child-session lifecycle. Dispatch depth and open sessions are bounded. A completed child response does not imply that its session closed.

Every spawned CLI, MCP, and Procedure process enters the Windows process-group boundary in `process_group.rs`. Parent shutdown closes owned child trees.

## Conversation and configuration state

The application actor owns the visible transcript, active operation, session selection, counters, settings projection, and service status. Backend stream events become ordered application changes.

Saved conversations remain under the project root. A mid-turn session switch is deferred. A new conversation or parent reset closes every child session still owned by the outgoing conversation.

`project_root` anchors configuration, sessions, policies, Procedure reports, and instructions. `working_dir` controls agent filesystem tools and CLI process directories. Changing `working_dir` never changes the process-wide current directory.

`SharedFlags` carries interrupt, effort, voice reply mode, context budget, model, working directory, repeat interrupt, and style settings across a root backend replacement. Subagents receive independent flags.

## Images and voice

`ImageAttachment` keeps MIME type plus base64 payload. Browser uploads accept supported image bytes after magic-byte detection and decoding. Text-only messages keep their original wire shape.

Voice uses whisper-rs for speech to text and Kokoro for text to speech. Missing model files disable the affected direction without blocking application startup. Model paths and prerequisites are documented in `docs/voice-setup.md`.

## Procedure and search workspaces

Autopilot repeats a task with a clean conversation per iteration. Cascade runs bounded candidates and selection. Evolve maintains a population and fitness archive. Procedure localizes, previews, verifies, reviews, and applies bounded changes.

Each workspace has one active operation identity. Stop commands address that identity. Late progress from an older operation cannot overwrite the current state.

## Build and test

Use these commands from the repository root:

```powershell
npm --prefix web ci
npm --prefix web run typecheck
npm --prefix web run lint
npm --prefix web test
npm --prefix web run build
cargo check --workspace
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace -- -D warnings
```

The generated asset manifest records SHA-256 digests for every embedded production file. The Rust build fails when a file is missing or its digest is stale.

Browser tests use `playwright-rs` and isolated loopback servers with deterministic service substitutes. They do not read the checkout's `settings.json` or call an external model. See `docs/browser-testing.md` for installation, commands, and failure artifacts.

The integration crate sets `autotests = false` and declares one `tests/it/main.rs` target. This avoids linking the large native and browser dependency graph into one executable per test file.

Model-dependent voice tests remain behind the `voice-models` feature. Normal workspace tests do not require downloaded voice models or audio hardware.
