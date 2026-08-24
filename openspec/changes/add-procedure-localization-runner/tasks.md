## 1. Procedure state and settings

- [x] 1.1 Create `crates/deepseek-custom/src/procedure/` with typed run, stage, scratchpad, attempt, target, and terminal-disposition types, then expose the module from `lib.rs`.
  <!-- status: completed -->
- [x] 1.2 Add the optional procedure settings block with localization backend and repository-index limits, plus load, merge, mutation, and round-trip coverage in `crates/deepseek-custom-tests/tests/it/config_settings.rs`.
  <!-- status: completed -->
- [x] 1.3 Add per-run JSON report storage under `.deepseek/procedure-runs/` and cover save, load, missing directory, and corrupt report behavior.
  <!-- covers: deepseek-custom/procedure-localization :: Localization is observable and non-mutating :: Completed report is inspectable -->
  <!-- status: completed -->

## 2. Stage 0 OpenSpec input

- [x] 2.1 Resolve and invoke `openspec validate <change> --strict --no-interactive` with the current Windows command resolution rules, capturing its exact failure.
  <!-- covers: deepseek-custom/procedure-localization :: A procedure run starts from a valid OpenSpec change :: Invalid change stops before model use -->
  <!-- status: completed -->
- [x] 2.2 Load active changes, unchecked tasks, `covers` bindings, and the smallest requirement or capability slice needed by the selected task.
  <!-- covers: deepseek-custom/procedure-localization :: A procedure run starts from a valid OpenSpec change :: Valid change starts localization -->
  <!-- status: completed -->
- [x] 2.3 Build the typed localization prompt from only the spec slice, repository index, and scratchpad, with the target-selection instruction at the prompt boundary.
  <!-- covers: deepseek-custom/procedure-localization :: Localization context is short and typed :: Captured request contains only stage context -->
  <!-- status: completed -->
- [x] 2.4 Add prompt-capture assertions for instruction placement and the absence of unrelated conversation history.
  <!-- covers: deepseek-custom/procedure-localization :: Localization context is short and typed :: Key instruction is not buried -->
  <!-- status: completed -->

## 3. Repository index

- [x] 3.1 Walk the current working directory into normalized repository-relative paths while excluding `.git`, `target`, `.deepseek`, binary files, and reparse-point traversal.
  <!-- status: completed -->
- [x] 3.2 Extract conservative Rust item symbols per indexed path and support path-only targets for other file types.
  <!-- status: completed -->
- [x] 3.3 Validate every returned path and optional symbol against the current index and report all rejected targets together.
  <!-- covers: deepseek-custom/procedure-localization :: Every localization target exists :: All reported targets are valid -->
  <!-- status: completed -->
- [x] 3.4 Add regression fixtures for traversal, absolute paths, invented files, mismatched symbols, Unicode paths, and index-size limits.
  <!-- covers: deepseek-custom/procedure-localization :: Every localization target exists :: A target is invented -->
  <!-- status: completed -->

## 4. Structured local dispatch

- [ ] 4.1 Add optional typed `response_format` support to `ChatRequest` without changing normal agent request serialization.
- [ ] 4.2 Map the localization JSON Schema onto Ollama's OpenAI-compatible request and add a captured-wire integration test.
  <!-- covers: deepseek-custom/procedure-localization :: Localizer output is schema constrained :: Ollama receives the localization schema -->
- [ ] 4.3 Reject non-Ollama localization backends before dispatch and cover the error for Api, Claude CLI, and Codex CLI entries.
  <!-- covers: deepseek-custom/procedure-localization :: Localizer output is schema constrained :: Backend cannot constrain output -->
- [ ] 4.4 Implement the tool-free, non-streaming localization call and decode its final content into the typed envelope.

## 5. Bounded localization run

- [ ] 5.1 Implement the Stage 0 to Stage 1 runner with progress events, interrupt checks, fingerprints, and report finalization.
- [ ] 5.2 Retry one malformed or invalid localization with the exact validation error and accept a valid second result.
  <!-- covers: deepseek-custom/procedure-localization :: Invalid localization has one bounded retry :: Retry repairs invalid output -->
- [ ] 5.3 Stop after two invalid results and assert that no third model call occurs.
  <!-- covers: deepseek-custom/procedure-localization :: Invalid localization has one bounded retry :: Retry budget is exhausted -->
- [ ] 5.4 Add a full stub-backed run test that hashes the fixture workspace before and after success, failure, and interruption.
  <!-- covers: deepseek-custom/procedure-localization :: Localization is observable and non-mutating :: Workspace remains unchanged -->

## 6. Procedure interface and verification

- [ ] 6.1 Add the Procedure tab with active-change, task, and localization-backend controls, a Run button, progress, targets, evidence, and final status.
- [ ] 6.2 Wire Procedure events and interruption through the existing GUI event path without adding messages to chat history.
- [ ] 6.3 Run a real read-only localization against `implement-hemisphere-model` with the configured Ollama backend and record the observed result in the change notes.
- [ ] 6.4 Run `cargo fmt --all -- --check`, `cargo check --workspace`, `cargo clippy --workspace -- -D warnings`, and `cargo test --workspace`.
