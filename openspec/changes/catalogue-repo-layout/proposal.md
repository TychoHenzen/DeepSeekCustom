## Why

The repository root holds 34 entries and nobody can say from memory which ones are source, which ones a tool writes, and which ones are dead. A survey run for this change found four empty agent-config directories, three stray log files that only a machine-global ignore file hides, a hook dependency (`time.ps1`) that two settings files call and `.gitignore` excludes, and 2.4 GB of build and scratch output. Cleaning that up one entry at a time needs a written inventory first, otherwise each pass re-derives the same facts.

## What Changes

- Add a checked-in catalogue at `docs/notes/repo-layout.md`, one row per top-level entry, naming what writes it, whether git tracks it, and its disposition (keep, ignore, delete, needs a decision).
- State the repo-hygiene rules the catalogue is checked against as requirements, so a later entry has a rule to be judged by rather than a case-by-case opinion.
- Fix the concrete violations the survey found: the empty agent-config directories, the stray root logs, the `time.ps1` tracking contradiction, and the missing `.gitignore` entries for directories that a workflow writes.
- Record the entries that need a human decision (the 28 MB of saved conversations under `.deepseek/`, the 19 MB `mutants.out`, the 5.9 MB `.code-review-graph/graph.db`) as approval items, not as unconditional deletions.

Nothing under `crates/` changes. No production Rust source is touched.

## Capabilities

### New Capabilities
- `repo/layout-catalogue`: the repository layout catalogue and the hygiene rules it enforces. What must be documented, what must be ignored, and what must not sit untracked in the root.

The group is `repo` rather than `deepseek-custom` or `deepseek-custom-tests` on purpose. This capability governs the repository around both packages and belongs to neither. `openspec/specs/` is empty today, so this change sets the naming precedent rather than following one.

### Modified Capabilities

None. `openspec/specs/` holds no capabilities yet.

## Impact

- New file: `docs/notes/repo-layout.md`.
- Edited: `.gitignore`, and `CLAUDE.md` plus `AGENTS.md` if a documented path moves. CLAUDE.md states those two are the same guide and get edited together.
- Deleted: empty agent-config directories and stray root logs, subject to the tasks below.
- Not touched: `crates/` internals. `docs/notes/quality-checklist.md` and the `quality-refactor` skill already own source-level cleanup, and re-planning it here would duplicate live machinery.
- No Rust code, no dependency, and no build setting changes, so `cargo test --workspace` is a regression check rather than the point.
