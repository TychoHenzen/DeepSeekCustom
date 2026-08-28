## Context

The existing procedure code separates read-only localization, sampled execution, isolated verification, promotion, and durable reports. Existing tests cover these stages independently and include stub-backed whole-change cases, but the reported failure shows that a successful model response can still fail semantic target validation before the later stages run. The acceptance fixture must therefore exercise the production composition in a disposable project and retain the failure boundary.

## Goals / Non-Goals

**Goals:**

- Prove one small OpenSpec task through the complete current procedure path.
- Reproduce the invalid-symbol failure deterministically and assert its exact bounded behavior.
- Capture ordered dispatch and verifier observations that identify the first failed stage.
- Keep all generated files, reports, and promotions inside a temporary sandbox.

**Non-Goals:**

- This change does not make a live Ollama or Codex call a repository test dependency.
- This change does not change localization semantics, retry budgets, routing policy, or production configuration.
- This change does not complete or archive `add-routing-sampling-and-metrics`.
- This change does not modify the user's existing `settings.json` or any repository source file during the test.

## Decisions

### Use a temporary project with a real OpenSpec shape

The fixture will create the smallest project that the current OpenSpec loader and validator accept. It will include a Rust target with one indexed symbol, an unrelated file, and a one-task change whose spec and task binding agree. This catches path, symbol, task-selection, and validation wiring errors that hand-built in-memory state cannot catch.

Alternative considered: reuse the repository's active OpenSpec change. Rejected because that couples the acceptance test to changing task counts, active work, and unrelated dirty files.

### Drive the existing production seams with deterministic dispatchers

The test will provide recording localization, patch, frontier, and verifier seams through the existing external test interfaces. The passing case returns a valid path-only or exact-symbol target and a valid mechanical patch. The failure cases return the known invalid symbol and then either repeat it or repair it. Every dispatcher records calls, prompts or typed requests where already exposed, and stage order without storing credentials or source content in long-lived artifacts.

Alternative considered: invoke a live provider in the default test suite. Rejected because provider availability, model output, network state, and credentials would make the acceptance result non-deterministic and unsuitable as a repository gate.

### Assert both state transition and durable evidence

The test will assert the terminal outcome, promoted bytes, untouched bytes, dispatch counts, verifier ordering, and reloaded report fields. This prevents a test from passing only because a helper returned success while the report, promotion, or failure diagnostics were disconnected.

Alternative considered: assert only the final target contents. Rejected because that would miss skipped verification, unintended frontier use, missing report persistence, and the immediate localization failure described by the user.

### Compare the real checkout around the sandbox run

The fixture will snapshot relevant real-repository bytes or hashes before execution and compare them afterward. It will specifically preserve the pre-existing dirty `settings.json` state. Temporary-directory cleanup will happen after assertions and will not use repository-wide deletion.

## Risks / Trade-offs

- [Risk] OpenSpec CLI behavior or fixture syntax changes independently of the Rust procedure code -> Mitigation: keep the proposal minimal, validate it through the same command path, and assert the exact preflight result.
- [Risk] Windows process and temporary-directory behavior causes parallel test interference -> Mitigation: use unique temporary roots and run the focused procedure acceptance test serially during verification.
- [Risk] A fake dispatcher can hide provider transport problems -> Mitigation: retain the existing provider smoke coverage separately and use this fixture only for deterministic end-to-end lifecycle and semantic-boundary coverage.
- [Risk] Captured prompts could leak source content into test diagnostics -> Mitigation: assert typed stage facts and bounded error strings, not full prompt bodies or raw model output.
