## Context

See proposal.md for motivation and `specs/deepseek-custom/procedure-localization/spec.md` for behavior. The current harness already has named backend resolution, one-shot API calls, shared interrupt flags, persistent GUI settings, and routed progress events. It does not have a typed multi-stage coding run or a structured-response field on `ChatRequest`.

The OpenSpec CLI is the source of truth for change validation. The existing Windows process rules require command resolution through `PATH` and `PATHEXT` before spawning. The production crate already depends on `walkdir`, `regex`, `serde_json`, and `uuid`.

## Goals / Non-Goals

**Goals:**

- Establish the persistent run model and GUI seam later milestones can extend.
- Make the first milestone useful without granting it write access.
- Keep model input short and make final machine-readable output structurally valid.

**Non-Goals:**

- Drafting or applying patches.
- Supporting schema constraints on every backend type.
- Indexing full language semantics. Rust symbols are indexed in this milestone. Other files remain valid path-only targets.

## Decisions

### Add a procedure subsystem beside search

Create `src/procedure/` with modules for run state, OpenSpec input, repository indexing, structured dispatch, reports, and progress. `ProcedureRun` owns the change id, selected task, spec fingerprint, repository fingerprint, `ProcedureScratchpad`, stage, attempts, and terminal disposition.

This stays separate from `search/cascade`. Cascade answers one prompt and votes on text. A procedure run carries typed state across ordered stages and later owns disposable workspaces. Shared backend and event primitives remain reusable.

### Select one OpenSpec task and its smallest contract slice

The Procedure tab lists active changes and their unchecked tasks. A selected task with a `covers` annotation loads that requirement and scenario. A task without a binding loads the task text plus the capability delta it belongs to. The prompt also carries a compact proposal scope and the typed scratchpad. It never carries the chat transcript or unrelated change artifacts.

Before reading this slice, invoke `openspec validate <change> --strict --no-interactive`. Resolve `openspec` through the existing Windows command resolver. Do not reproduce OpenSpec validation rules in Rust.

### Build a bounded repository index

Walk from the current `working_dir`, skip `.git`, `target`, `.deepseek`, ignored binary files, and reparse-point directory traversal. Store normalized repository-relative paths. Extract Rust item names with a conservative parser built around existing `regex` support. A symbol is optional in the output schema, so non-Rust files remain addressable.

The JSON Schema uses an enum for valid paths. Symbol membership is checked after decoding because a separate per-path symbol enum would make the schema large and hard to maintain.

### Use a tool-free Ollama structured dispatch

Add an optional typed `response_format` field to `ChatRequest`. `ApiClient::prepare_request` retains it only for Ollama and returns a clear unsupported-provider error before a procedure dispatch. Procedure localization uses non-streaming chat, temperature zero, no tools, and the configured effort.

The model may emit provider-separated reasoning, but the final content must match `LocalizationEnvelope`. This follows the procedure document's free-reasoning and constrained-final-answer boundary. A normal agent turn remains wire-compatible because its new field is `None`.

Alternative considered: prompt-only JSON plus extraction. Rejected because malformed structure is the exact local-model failure this milestone removes.

### Persist reports outside conversation history

Write one JSON report per run under `.deepseek/procedure-runs/<uuid>.json`. Send a small `ProcedureProgress` event for GUI state. Do not inject procedure internals into `MessageHistory`, because later retries must reconstruct fresh prompts instead of growing a transcript.

## Risks / Trade-offs

- [Large path enums can enlarge the prompt] -> Exclude generated trees and cap the index with a clear error that asks for a narrower working directory.
- [Regex symbol indexing is incomplete] -> Accept path-only targets and reject only a symbol the index claims does not exist.
- [Ollama versions differ] -> Add a captured wire test and a real optional smoke test against the configured local backend.
- [OpenSpec may be absent from PATH] -> Report the resolved command failure before any model request.

## Migration Plan

Add an optional `procedure` settings block and default it to no selected backend. Existing settings continue to load. Rollback removes the block and the new run files without changing conversations or source files.
