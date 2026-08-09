# Autopilot task text for the Diversity checklist

Paste the block below into the Autopilot tab's task box. Set the iteration
count to 32: the checklist holds 30 items, and the run needs a spare turn or
two for a retry.

Each iteration starts with no conversation history. The checklist file on
disk is the only thing that carries across, which is why the task text below
makes the agent read it first every time.

## The task text

```
Work through docs/plans/2026-08-09-diversity-implementation-checklist.md, one item per run.

You have no memory of any earlier run. The checklist file on disk is the only record of what is already done. Read it before you do anything else.

1. Read the checklist. Find the first item whose box is unchecked, reading top to bottom. The phases are in build order, so never skip ahead to a later item.
2. If every box is checked, reply with the exact words ALL ITEMS DONE and stop. Change no file.
3. Read docs/plans/2026-08-09-diversity-implementation-plan.md for the reasoning behind that one item, and CLAUDE.md for the architecture it lands in. Read the source files the item names before you edit them.
4. Implement that one item and nothing else. Do not start the next item, even when it looks small. Do not refactor code the item does not name.
5. Verify, in this order:
   cargo check -p deepseek-custom
   cargo test -p deepseek-custom-tests --test it <filter>
   cargo clippy --workspace -- -D warnings
   Never run cargo build, cargo run, or a bare cargo test. You are running inside target/debug/deepseek-custom.exe right now, and Windows holds a lock on that file while it runs. Any command that links the binary fails with a linker error naming the exe, and it is not a fault in your change. The three commands above never link it: cargo check and cargo clippy do not link at all, and the --test it target builds only the test binary and the library beside it.
   The test filter is the test file that covers the module you changed. The naming rule is in the Tests section of CLAUDE.md: take the module path under crates/deepseek-custom/src/, drop a trailing /mod.rs or .rs, join the segments with an underscore. So agent/prompt.rs is covered by agent_prompt. Name the target first, as --test it, then the filter.
6. If any of those three commands fails, fix it in this same run. Do not check the box. Do not leave the build broken.
7. Only once all three pass, edit the checklist and change that one item's [ ] to [x]. Change no other box.
8. Append one line to .autopilot/step-log.md, creating the file if it is not there: the item id, one sentence on what changed, and the passing test count. Keep it to one line.
9. Reply with the item id, the files you changed, and the last line of the test output.

Rules that hold for every run:
- One checklist item per run. The checkbox is the handoff to the next run.
- Never ask a question. Decide, act, and say in your reply what you decided and why.
- Never write an em dash or any other non-ASCII punctuation. Plain ASCII only.
- Never mark an item done on a broken build or a failing test.
- Tests live in crates/deepseek-custom-tests. The production crate crates/deepseek-custom holds no test code at all. A test that needs a private item turns the test-support feature on rather than making the item public.
- A new test file must be declared with a mod line in crates/deepseek-custom-tests/tests/it/main.rs, or it will not run and will not be compiled.
- A change to the GUI paint path cannot be seen until the app is rebuilt, and the app cannot be rebuilt while it is running. Make the change, verify it with cargo check and the tests, check the box, and say in your reply that the change needs a restart to be visible.
```

## The policy file

`autopilot-policy.md` in the project root answers any `AskUserQuestion` call
the agent makes. Without it the answerer falls back to the first offered
option every time, silently. That file is checked in alongside this one.

## Watching a run

The Autopilot tab draws the same transcript blocks the Chat tab draws, so
each iteration's output is visible as it goes. Escape stops the whole run,
not just the iteration in flight.

Two files record what happened. `.autopilot/decisions.log` holds one line per
`AskUserQuestion` the answerer resolved. `.autopilot/step-log.md` holds one
line per finished checklist item, written by the task text above. Both sit
under `.autopilot/`, which `.gitignore` already excludes.

## When a run stalls

An iteration that cannot finish its item leaves the box unchecked, so the
next iteration picks up the same item from the start. That is the intended
behavior for a flaky failure. It is the wrong behavior for a real blocker,
where every remaining iteration will retry the same item and get the same
result. Watch the step log. Two iterations naming the same item mean the
item needs a human.
