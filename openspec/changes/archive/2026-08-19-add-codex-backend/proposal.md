## Why

The harness supports two backend kinds today: `Api` (DeepSeek/Ollama via HTTP) and `ClaudeCli` (`claude -p` via stream-json). OpenAI's Codex CLI has a non-interactive `exec` mode that outputs JSONL events, handles its own tools internally, and supports session resume - the same shape as `claude -p`. Adding it as a third subprocess backend lets the harness drive OpenAI models through one consistent GUI, with the same transcript, autopilot, and subagent dispatch that the other two backends already have.

## What Changes

- Add a `CodexCli` variant to `BackendConfig` and `Backend`, alongside `Api` and `ClaudeCli`.
- Add a `codex_cli` module under `backend/` that spawns `codex exec --json`, parses the JSONL event stream, and maps each event type to the existing `StreamEvent` enum.
- Add a `codex_cli` variant to `BackendConfig` in `settings.rs`, accepting `model`, optional `sandbox`, optional `env`, and optional `models`.
- Wire the new variant through `BackendFactory::build`, `Backend::run`, `Backend::run_repeat`, `Backend::adopt_flags`, and `Backend::shutdown`.
- Wire model discovery for Codex through `list_models`.
- Add the new variant to the backend picker dropdown.
- Add the child process to the Windows job object through `process_group::adopt`, the same way `claude -p` children are adopted.

## Capabilities

### New Capabilities
- `deepseek-custom/codex-backend`: The Codex CLI subprocess backend - spawning, JSONL event parsing, event mapping, session resume, effort control, image input, autopilot repeat, subagent dispatch, and working directory support.

### Modified Capabilities

(none)

## Impact

- `crates/deepseek-custom/src/backend/mod.rs`: new `CodexCli` variant on `Backend` enum, new arms on every match.
- `crates/deepseek-custom/src/backend/factory.rs`: new build path in `BackendFactory::build`.
- `crates/deepseek-custom/src/backend/codex_cli/`: new module directory with process management, event parsing, and event mapping, structured the same way `claude_cli/` is.
- `crates/deepseek-custom/src/config/settings.rs`: new `CodexCli` variant on `BackendConfig`.
- `crates/deepseek-custom/src/api/models.rs`: new discovery path for Codex models.
- `crates/deepseek-custom/src/effort.rs`: new mapping column for Codex reasoning effort values.
- `crates/deepseek-custom-tests/`: new test files covering the event parser, event mapper, spawn arguments, and lifecycle, following the same patterns as the `claude_cli` tests.
- No new crate dependencies beyond what the workspace already carries (`serde_json`, `tokio`, `reqwest` for model discovery).
