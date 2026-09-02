## Why

Top-level development turns can currently change the real workspace before the user reviews a bounded plan. Prompt instructions alone cannot enforce that boundary across API, Claude CLI, and Codex CLI backends.

## What Changes

- Add an optional Controlled Development mode with the phases `Off`, `Planning`, `AwaitingApproval`, `Executing`, `Completed`, `Blocked`, and `Interrupted`.
- Require planning to produce one strictly decoded Work Card and prevent planning from changing the real workspace or dispatching subagents.
- Bind approval to one card, execute that card in a disposable snapshot, and point every backend at that snapshot through its working-directory boundary.
- Reuse Procedure snapshot, verification, fingerprint, interrupt, and transactional promotion seams to enforce exact approved paths, a three-file production limit, dependency-change exceptions, proof commands, and concurrent-edit rejection.
- Retain unexpected isolated changes as diagnostic evidence instead of deleting or promoting them.
- Persist card and phase with the owning session, convert restored in-flight work to `Interrupted`, and clean up session-owned disposable workspaces on reset or deletion.
- Rewrite the bounded root `PROJECT_STATE.md` snapshot only after successful promotion.
- Add compact controls and summaries to the existing web application while preserving complete raw backend details behind a collapsed view.
- Add deterministic integration, fake Codex CLI, frontend, and real production Ollama browser coverage without adding dependencies.

## Capabilities

### New Capabilities

- `deepseek-custom/controlled-development-mode`: Defines Work Cards, lifecycle enforcement, isolated execution, promotion gates, session recovery, control commands, project-state output, and the existing-page UI.

### Modified Capabilities

None.

## Impact

- Affects application DTOs and command arbitration, session records, backend construction and prompts, Procedure-owned isolation and promotion seams, and the existing React workspace.
- Adds external integration coverage in `crates/deepseek-custom-tests` and frontend coverage in `web`.
- Adds `PROJECT_STATE.md` as harness-owned current state after a successful packet.
- Adds no dependency, frontend framework, storage service, patch engine, process-group implementation, or workspace snapshot system.
- Normal chat and existing Procedure behavior remain unchanged when Controlled Development mode is off.
