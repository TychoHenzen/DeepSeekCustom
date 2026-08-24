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
