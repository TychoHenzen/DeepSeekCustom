---
name: taskrunner
description: Autonomous sequential task executor for implementation plans. Reads a plan file with checked/unchecked tasks, picks the next unstarted task respecting phase dependencies, marks start time before work, implements the task, marks end time and checks it off upon verified completion. Designed for /loop invocation — each call processes exactly one task then exits. Use when the user says "run the plan", "execute tasks", "start taskrunner", "/loop taskrunner", "work through the plan", "autonomous implementation", or wants to batch-execute plan tasks without supervision.
compatibility: rust, git, cargo
---

# Taskrunner — Autonomous Plan Executor

Reads an implementation plan markdown file, finds the next unchecked task, executes it, and updates the plan with timestamps. One task per invocation. Designed to run under `/loop` for fully autonomous execution.

## Plan file

The plan is at `docs/plans/2026-05-27-deepseek-harness-implementation-plan.md`. Read it fresh every invocation — plan state changes each cycle.

## Task format

Tasks in the plan look like:

```
- [ ] T1.1.1 Create `src/main.rs` — entry point, tokio runtime bootstrap | Start: --- | End: ---
```

After completion they become:

```
- [x] T1.1.1 Create `src/main.rs` — entry point, tokio runtime bootstrap | Start: 2026-05-27 09:15 | End: 2026-05-27 09:23
```

## Execution sequence (one task per invocation)

### Step 1: Scan for next task

Read the plan file. Find the FIRST line matching `- [ ] T` that is NOT blocked by an incomplete phase dependency.

**Phase dependency check:** Each phase header (`# Phase N:`) marks a boundary. A phase may depend on prior phases (shown in the "Phase Dependencies" graph at the bottom of the plan). The rule:

- Phase 0 has no dependencies — always eligible
- Phase 1 requires all Phase 0 tasks to be `[x]` (done)
- Phase 2 requires all Phase 1 tasks to be `[x]` (done)
- Phase 3 and Phase 4 can run in parallel after Phase 2 — both require all Phase 2 tasks done

If the next unchecked task is in a blocked phase, report: "Phase N blocked — Phase N-1 has X incomplete tasks" and stop. Don't skip ahead.

If NO unchecked tasks remain across all phases, report: "All tasks complete" and stop.

### Step 2: Mark start time

Get current time. On Windows: `powershell -Command "Get-Date -Format 'yyyy-MM-dd HH:mm'"`. On Unix: `date '+%Y-%m-%d %H:%M'`.

Update the plan file: replace `| Start: ---` with `| Start: <current time>` on the selected task's line. Use exact string replacement (the line is unique — the task ID ensures that).

### Step 3: Implement the task

Read the task description. Understand what it asks for:
- New files to create, code to write, tests to add
- Follow the patterns and conventions shown in the plan's code blocks
- If the task references a code block in the plan (e.g., "Create `src/error.rs` with `HarnessError` enum"), write that exact code

Execute the implementation:
- Write code files using Write/Edit tools
- Add dependencies to `Cargo.toml` if the task specifies them
- Write tests if the task says to
- Run `cargo check` or `cargo test` to verify

**Important:** Do exactly what the task says. Don't expand scope. Don't implement adjacent tasks. One task only.

### Step 4: Verify

Run the verification command specified in the task (typically `cargo check` or `cargo test`). If it passes, proceed to Step 5. If it fails:

- Attempt to fix the issue — the task's own code should compile
- If fixable within the task scope, fix and re-verify
- If unfixable (e.g., depends on code from a future task), mark the task as blocked rather than failed

### Step 5: Mark end time and complete

Get current time again (same method as Step 2).

Update the plan file on the task's line:
1. Replace `| End: ---` with `| End: <current time>`
2. Replace `- [ ]` with `- [x]`

Use TWO separate Edit calls (or a single sed if both changes on one line).

### Step 6: Report

Output exactly one status line:

```
T1.1.1 done (Start: 09:15, End: 09:23). Next: T1.1.2
```

If all tasks complete: "All tasks complete. Plan finished."

## Failure handling

If a task cannot be completed after reasonable effort:

1. Replace `- [ ]` with `- [!]` (marks as failed)
2. Keep the start time, set end time to current time
3. Append failure reason after the end time: `| FAIL: <reason>`
4. Report: `T1.1.1 FAILED: <reason>. Next: T1.1.2`
5. Move to next task

Failures should be rare — most tasks are straightforward code creation.

## Idempotency

Each invocation reads the plan fresh. If a task was already started (has Start time but no End time from a crashed previous invocation), resume it: skip the start-time step, re-implement, verify, mark end time.

If a task is already `[x]`, skip it and move to the next.

## Phase completion check

After completing the LAST task in a phase (the last `T#.#.#` under a `## T#:` heading before the next `---` or `# Phase` marker), check if the next task is in a new phase. If so, verify phase dependency — the next task may legitimately be in the next phase.
