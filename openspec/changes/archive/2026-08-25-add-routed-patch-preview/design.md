## Context

This change builds on the persistent run, typed scratchpad, repository index, and structured Ollama dispatch from milestone 1. `BackendFactory` already resolves Api, Claude CLI, and Codex CLI backends. `run_subagent` can dispatch them, but CLI agents can use tools, so they must never run against the real workspace during preview generation.

## Goals / Non-Goals

**Goals:**

- Produce one conservative, explainable route decision.
- Produce one parseable patch preview from either model tier.
- Preserve the real workspace even when a CLI backend uses tools.

**Non-Goals:**

- Compiling, testing, or promoting a patch.
- Learned routing or token-confidence routing.
- General-purpose semantic difficulty prediction.

## Decisions

### Gate preview on one named approved localization report

The preview request carries the localization run ID. Load that exact report through the approved-report guard before evaluating difficulty signals, creating a disposable workspace, or dispatching a drafting model. The report must be `Approved`, belong to the selected change and task, and retain matching OpenSpec and target fingerprints. Reject `Pending`, `Rejected`, `LegacyUnreviewed`, missing, stale, and change- or task-mismatched reports without drafting a patch. Do not fall back to the newest report or another run.

### Make the router a pure rule table

`DifficultyAssessment` contains a `RouteTier`, an ordered list of `RouteSignal`, and an optional override. Local routing requires all of these conditions: one localized file, an explicitly mechanical task verb, and no frontier marker. Mechanical verbs cover rename, import, signature propagation, boilerplate, test scaffolding, formatting, and documentation. Frontier markers cover architecture, cross-cutting behavior, concurrency, security, migration, public API change, subtle bug language, and substantive logic.

Unknown or conflicting text routes to frontier. This is intentionally conservative. The ordered signal list makes a test and the GUI explain the same decision.

Alternative considered: ask the local model to classify difficulty. Rejected because that would make the escalation gate depend on the weak model's self-assessment.

### Use one patch envelope and one validator

`PatchEnvelope` holds target paths, a short rationale, and a unified diff. The local path uses JSON Schema. API frontier models use JSON mode when available. CLI frontier models must return one JSON object, which the same parser validates after dispatch.

Validation normalizes path separators, rejects absolute paths and traversal, parses every old and new diff path, and compares them to the localization allowlist. Run `git apply --check` against a disposable draft workspace to catch malformed hunks without changing files.

### Run every drafting backend in a disposable workspace

Add a minimal snapshot helper that copies current source state while excluding `.git`, `target`, and `.deepseek`. Point `SubagentRequest.working_dir_override` at that snapshot for Claude and Codex drafting. A CLI agent can read or edit its copy, but only its final parsed envelope survives. The snapshot is deleted after the preview result.

Milestone 3 extends this helper with verifier execution and promotion. Building the isolation seam here is required to keep this milestone's non-mutation contract.

### Fingerprint preview inputs

Store SHA-256 fingerprints for the selected OpenSpec slice and every localized target. Add `sha2` as a direct dependency. Preview refuses a stale localization report. The hashes also become milestone 3's concurrent-edit guard.

### Keep overrides explicit and local to one run

The GUI offers Automatic, Force local, and Force frontier. An override changes only that preview request. It does not rewrite routing settings or future runs. The report retains both the automatic and effective tier.

## Risks / Trade-offs

- [Keyword rules miss some easy work] -> Unknown work routes to frontier, and reports expose signals for later tuning.
- [CLI output includes commentary] -> The parser requires exactly one envelope and reports a structural error instead of guessing.
- [Snapshot copy adds latency] -> Exclusions avoid generated trees, and the copy is smaller than running verifiers.
- [A local override can be unsafe] -> It remains preview-only in this milestone and is visibly marked.

## Migration Plan

Extend the optional procedure settings with local and frontier backend names. Existing localization reports remain readable but require a new preview run after target fingerprinting is introduced.
