## Why

DeepSeekCustom can switch backends and run answer cascades, but it cannot execute the staged coding procedure in `docs/IntelligenceProcedure.md`. The first useful milestone is a read-only run that validates an OpenSpec change, builds a short typed context, and asks the local model to identify real repository targets.

## What Changes

- Add a Procedure tab that accepts an existing OpenSpec change and a localization backend.
- Validate the selected change before any model call. Stop with the exact validation error when Stage 0 is not ready.
- Build a repository index of real files and Rust symbols. Send only the relevant spec slice, index, and typed scratchpad to the localizer.
- Add JSON Schema constrained output support for Ollama through its OpenAI-compatible `response_format` field.
- Validate every returned path and symbol against the repository index. Retry one malformed or invalid localization result, then stop without changing the workspace.
- Save a read-only localization report that names the selected files, symbols, evidence, backend, model, and validation result.
- Keep this milestone non-mutating. It produces no patch and runs no edit command.

## Capabilities

### New Capabilities

- `deepseek-custom/procedure-localization`: A runnable Stage 0 and Stage 1 flow that validates an OpenSpec change and produces a schema-constrained localization report over real repository targets.

### Modified Capabilities

None.

## Impact

Production work will add a `procedure` module, Procedure GUI state, procedure progress events, and settings for the localization backend. `api/types.rs` and `api/client.rs` will gain provider-gated structured response support. Tests will live in `crates/deepseek-custom-tests/tests/it/` and will cover OpenSpec validation, repository indexing, schema request mapping, invalid-target retry, interruption, and a complete read-only run.

This is milestone 1 of 5. A practical test is to select `implement-hemisphere-model`, run the configured Ollama 7B backend, and inspect the validated localization report. The later milestones consume that report.
