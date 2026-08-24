# Procedure localization visual verification

This checklist verifies the rendered Procedure view for localization review. The evidence is captured from the production `ProcedureTab` render path through egui and wgpu.

## Capture setup

Use the repository root as the working directory. The capture harness requires the `test-support` feature and creates its disposable OpenSpec fixture under `target/procedure-visual-capture/`.

The harness uses these deterministic values:

- Window content size: 1280 by 900 pixels.
- Change: `harden-procedure-localization`.
- Task: `4.1 Render Procedure review states`.
- Backend: `ollama-local`.
- Model: `qwen2.5-coder:7b`.
- Accepted targets: one target with `ProcedureTab::render_result` and one target without a symbol.

Build the headless harness once:

```powershell
cargo build -p deepseek-custom-tests --example procedure_visual_capture
```

Capture every state:

```powershell
.\target\debug\examples\procedure_visual_capture.exe running docs\evidence\procedure-localization\running.png
.\target\debug\examples\procedure_visual_capture.exe awaiting-review docs\evidence\procedure-localization\awaiting-review.png
.\target\debug\examples\procedure_visual_capture.exe approved docs\evidence\procedure-localization\approved.png
.\target\debug\examples\procedure_visual_capture.exe rejected docs\evidence\procedure-localization\rejected.png
.\target\debug\examples\procedure_visual_capture.exe failed docs\evidence\procedure-localization\failed.png
.\target\debug\examples\procedure_visual_capture.exe interrupted docs\evidence\procedure-localization\interrupted.png
```

Each process uses fixed egui input with a 1280 by 900 viewport, 1.0 pixels per point, and time 1.0. It tessellates the production widget and renders into a no-surface wgpu texture. It reads that texture into a PNG without creating or focusing an OS window.

## State checklist

### Running

Evidence: [`running.png`](evidence/procedure-localization/running.png)

Action: Start localization and wait for the first model attempt.

Expected state:

- [x] `Status: running` is visible with a progress spinner.
- [x] `Localization attempt 1 of 2`, backend, model, and observed attempt count are visible.
- [x] Stop is available while Run and the selection controls are disabled.
- [x] No result or review action is shown before a report exists.

Inspection: The progress block is readable. The spinner, controls, and labels do not overlap. No text is clipped.

### Awaiting review

Evidence: [`awaiting-review.png`](evidence/procedure-localization/awaiting-review.png)

Action: Finish schema and index validation with a structurally valid target set.

Expected state:

- [x] `Final status: awaiting review` and `Review: pending` are visible.
- [x] Backend, model, attempt count, and accepted disposition are visible.
- [x] Both target paths are visible.
- [x] `ProcedureTab::render_result` is visible for the target with a symbol.
- [x] The target without a symbol has no artificial symbol suffix.
- [x] Evidence is visible below each target.
- [x] Approve and Reject are both available.

Inspection: Both evidence lines and the complete report path fit inside the window. Approve and Reject remain separated and unclipped.

### Approved

Evidence: [`approved.png`](evidence/procedure-localization/approved.png)

Action: Approve the displayed awaiting-review run.

Expected state:

- [x] `Final status: approved` and `Review: approved` are visible.
- [x] The original targets, optional symbol, evidence, backend, model, and attempt details remain visible.
- [x] Approve and Reject are absent after the terminal decision.

Inspection: The retained target evidence and report path are readable. No control or text overlaps another element.

### Rejected

Evidence: [`rejected.png`](evidence/procedure-localization/rejected.png)

Action: Reject the displayed awaiting-review run.

Expected state:

- [x] `Final status: rejected` and `Review: rejected` are visible.
- [x] The inspected targets and evidence remain visible.
- [x] Approve and Reject are absent after the terminal decision.

Inspection: The rejected state is distinct from approval. The retained evidence is readable and unclipped.

### Failed

Evidence: [`failed.png`](evidence/procedure-localization/failed.png)

Action: Finish the run with an index-validation failure for an invented symbol.

Expected state:

- [x] The red `Final status: failed` diagnostic names `missing_symbol` and the repository index.
- [x] Backend, model, attempt count, and rejected attempt disposition are visible.
- [x] No invalid target is presented as accepted evidence.
- [x] No review action is available.

Inspection: The complete failure diagnostic fits on one line at the capture size. It does not overlap the dispatch details.

### Interrupted

Evidence: [`interrupted.png`](evidence/procedure-localization/interrupted.png)

Action: Interrupt localization while the first attempt is owned by the Procedure runner.

Expected state:

- [x] `Final status: interrupted` is visible.
- [x] Backend, model, attempt count, and interrupted attempt disposition are visible.
- [x] No review action is available.

Inspection: The interrupted state is distinct from failed and rejected. All visible text fits without overlap or clipping.

## Maintained evidence contract

The external test `procedure_visual_verification_manifest_requires_every_state_artifact` requires this checklist and all six PNGs. It rejects absolute paths, parent traversal, missing files, non-files, and paths that resolve outside the repository.

## Executable scenario coverage

The maintained Rust discovery entry is `crates/deepseek-custom-tests/tests/it/*.rs` under the `deepseek-custom` group in `openspec/test-globs.json`. The runner entry is `node scripts/run-rust-test-file.mjs` in `openspec/test-runners.json`.

Run the change coverage report:

```powershell
node "$env:USERPROFILE\mcp-servers\dod-guard\packages\dod-guard\dist\bundle.js" cover harden-procedure-localization
```

Observed result on 2026-08-24: `17 scenario(s): 17 bound, 0 unwired` and `cover OK - 0 regression(s)`.

Dod-guard generated seven distinct commands for the active delta. Each command ran its complete module, printed every bound test name, and passed:

| Generated command | Result | Bound tests observed |
|---|---:|---|
| `node scripts/run-rust-test-file.mjs "crates/deepseek-custom-tests/tests/it/procedure_input.rs"` | 10 passed | `one_capability_change_selects_the_complete_delta_for_an_unbound_task` |
| `node scripts/run-rust-test-file.mjs "crates/deepseek-custom-tests/tests/it/procedure_runner.rs"` | 14 passed | `multiple_capabilities_require_a_binding`, `configured_index_limits_reach_the_next_runner_without_replacement`, `repository_index_overflow_stops_before_model_dispatch`, `all_reported_targets_are_structurally_valid_and_await_review`, `invented_path_or_symbol_rejects_the_complete_localization_result`, `workspace_remains_unchanged` |
| `node scripts/run-rust-test-file.mjs "crates/deepseek-custom-tests/tests/it/config_settings.rs"` | 48 passed | `procedure_block_without_repository_index_uses_named_defaults` |
| `node scripts/run-rust-test-file.mjs "crates/deepseek-custom-tests/tests/it/procedure_index.rs"` | 16 passed | `supported_ascii_rust_item_kinds_expose_their_exact_identifiers`, `unicode_rust_identifier_remains_available_only_as_a_path_target` |
| `node scripts/run-rust-test-file.mjs "crates/deepseek-custom-tests/tests/it/procedure_dispatch.rs"` | 6 passed | `ollama_receives_the_localization_schema`, `backend_cannot_constrain_localization_output`, `ollama_model_lacks_native_thinking_control` |
| `node scripts/run-rust-test-file.mjs "crates/deepseek-custom-tests/tests/it/procedure_report.rs"` | 10 passed | `rejection_updates_only_the_named_awaiting_review_report_and_fails_the_approved_guard`, `approval_preserves_structural_evidence_and_is_the_only_path_through_the_guard` |
| `node scripts/run-rust-test-file.mjs "crates/deepseek-custom-tests/tests/it/gui_procedure_tab.rs"` | 11 passed | `approved_structural_report_round_trip_populates_the_complete_procedure_view`, `procedure_visual_verification_manifest_requires_every_state_artifact` |

The unchanged main-spec bindings also generated and passed:

```powershell
node scripts/run-rust-test-file.mjs "crates/deepseek-custom-tests/tests/it/procedure_prompt.rs"
```

Observed result: 3 passed, including both marked prompt tests.

Run repository coverage:

```powershell
node "$env:USERPROFILE\mcp-servers\dod-guard\packages\dod-guard\dist\bundle.js" cover --all
```

Observed aggregate result: 39 repository scenarios, 12 bound and 27 unwired, with `cover OK - 0 regression(s)`. The aggregate includes other capabilities, so it does not represent procedure-localization completeness.

The capability-specific structured results are:

- Main procedure-localization spec: 12 of 12 bound.
- Active delta: 17 of 17 bound.
- Main and delta overlap: 6 modified scenarios.
- Effective final procedure-localization spec: 23 of 23 unique scenario IDs bound.

The effective count is `12 + 17 - 6 = 23`. This is also `12` main scenarios plus `7` added-delta scenarios plus `4` new scenarios in modified requirements. The coverage ratchet remained unchanged.

## Live Ollama smoke

This evidence uses active change `implement-hemisphere-model`, task `1.1`, and the `ollama` backend. The run started from the repository root with:

```powershell
cargo run -p deepseek-custom --example procedure_localization_smoke
```

The example read `settings.json` at runtime. It used the configured model exactly as written:

```text
hf.co/mradermacher/Qwen2.5-7B-Instruct-1M-Thinking-Claude-Gemini-GPT5.2-DISTILL-GGUF:Q4_K_M
```

The read-only preflight reached Ollama `0.32.6`. The exact configured model appeared in `/api/tags`. The installation contained three models.

The report file timestamp was `2026-08-24T19:50:41.149Z`. The live result was:

- Report ID: `3e13ad71-35a7-4659-9a20-99fe3e0ff83e`.
- Report path: `.deepseek/procedure-runs/3e13ad71-35a7-4659-9a20-99fe3e0ff83e.json`.
- Attempt count: 2.
- Transport: passed. Ollama returned a response for both bounded attempts.
- Schema decoding: passed. Both responses decoded into localization target arrays.
- Repository structural validation: failed. Every returned Markdown target used the invented symbol `Select`.
- Semantic review: not reached. No structurally valid pending target set existed.

Attempt 1 returned these targets:

- `docs/agent-project-context.md`, symbol `Select`, evidence `docs/notes/claude-effort.md`.
- `docs/notes/claude-resume.md`, symbol `Select`, evidence `docs/notes/claude-effort.md`.
- `docs/notes/claude-thinking-display.md`, symbol `Select`, evidence `docs/notes/claude-effort.md`.

Attempt 2 returned the same three paths and symbol. Each target cited its own path as evidence. Both attempts were rejected with `symbol is not present under the indexed path` for every target.

The task contract names `crates/deepseek-custom/src/config/settings.rs` as the owner of `HemisphereSettings`. The returned targets did not point to that file. No approval was recorded for this live-model result.

## Source-mutation evidence

The aggregate covers every regular file under `crates/`. Paths are normalized to `/` and sorted with Node's default ordinal code-unit order. SHA-256 receives each normalized path, one NUL byte, then the file contents. Generated `.deepseek` reports are outside this source set.

Run the exact aggregate command from the repository root:

```powershell
node -e "const fs=require('fs');const path=require('path');const crypto=require('crypto');const walk=p=>fs.readdirSync(p,{withFileTypes:true}).flatMap(e=>e.isDirectory()?walk(path.join(p,e.name)):e.isFile()?[path.join(p,e.name)]:[]);const files=walk('crates').map(p=>p.split(path.sep).join('/')).sort();const h=crypto.createHash('sha256');for(const file of files){h.update(file);h.update(Buffer.from([0]));h.update(fs.readFileSync(file));}console.log(JSON.stringify({files:files.length,sha256:h.digest('hex')}));"
```

The live smoke used 268 source files. Its aggregate was identical before and after the model run:

```text
d49d0da9d9e148a61c566a3633936670fa3d2e8ef73ad37a999f5eeab4d1185a
```

The live result never reached an eligible review state. Separate controlled reports therefore exercise the production review storage operations. These are mutation checks, not live-model results. The helper validates the known target against the production repository index and sets `model: "not-dispatched"`.

The controlled target was `crates/deepseek-custom/src/config/settings.rs` with no symbol. Its evidence was `Task 1.1 names this file as the owner of HemisphereSettings.`

Approval command:

```powershell
cargo run -p deepseek-custom --example procedure_localization_smoke -- controlled-review approve
```

Approval result:

- Report ID: `a61ec86a-1550-49e4-9f5a-1fe8243474ee`.
- Report timestamp: `2026-08-24T20:08:43.658Z`.
- Report path: `.deepseek/procedure-runs/a61ec86a-1550-49e4-9f5a-1fe8243474ee.json`.
- Review disposition: `approved`.
- Model dispatch: false.
- Source aggregate before and after: `25e982dea23d838b7805814fa68a07091892847453fa1f0e998e5b503b3c3225` across 268 files.

Rejection command:

```powershell
cargo run -p deepseek-custom --example procedure_localization_smoke -- controlled-review reject
```

Rejection result:

- Report ID: `1bf8580f-43d3-43d1-b2b5-802efc0a3c46`.
- Report timestamp: `2026-08-24T20:08:59.358Z`.
- Report path: `.deepseek/procedure-runs/1bf8580f-43d3-43d1-b2b5-802efc0a3c46.json`.
- Review disposition: `rejected`.
- Model dispatch: false.
- Source aggregate before and after: `25e982dea23d838b7805814fa68a07091892847453fa1f0e998e5b503b3c3225` across 268 files.

The controlled aggregate differs from the earlier live-smoke aggregate because the maintained controlled-review command was added afterward. Each before-and-after comparison is internally identical. Approval and rejection changed only their named files under `.deepseek/procedure-runs/`.
