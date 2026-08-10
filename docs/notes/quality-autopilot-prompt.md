# Autopilot prompt: incremental quality refactor

Paste the block below into the Autopilot tab's task box. One iteration does one file.
Set the iteration count to how many files you want done in the run.

Each iteration starts with an empty conversation. The prompt carries every fact an
iteration needs, and the checklist file on disk is the only state that crosses between
iterations.

---

```
Refactor exactly one file toward the project's code quality bounds. Do the whole job for
that one file, then stop. Do not start a second file.

1. Read docs/notes/quality-checklist.md. Take the first unchecked box under
   "Production files". If that section has none left, take the first unchecked box under
   "Test files". If both sections are fully checked, write "checklist complete" and stop
   without changing anything.

2. Read the file you picked, plus its row in the "Detail" table at the bottom of the
   checklist. The row names which rules it breaks and its worst measurements.

3. Rescan it to see the exact violations, with line numbers:

   node C:\Users\siriu\.claude\plugins\cache\dod-guard\quality-guard\037c77ae9669\skills\quality-refactor\scripts\quality-scan.mjs <path> --root=.

4. Fix the violations. The bounds are: line 120 characters, file 300 lines, function 60
   lines, cyclomatic complexity 10, 7 parameters, nesting depth 5, one type per file, no
   unnamed tuple, no unused local, no commented-out code, no duplicated block. Prefer the
   tighter targets where they cost nothing: line 80, file 100 lines, function 30 lines,
   complexity 5, 3 parameters, nesting depth 3.

   Rules of the refactor:
   - Behavior must not change. This is a structural pass, not a bug fix and not a feature.
   - Splitting a file means creating new files by responsibility, and moving code into
     them. It does not mean deleting code or trimming the line count.
   - A new file goes in the same module directory, and gets a `mod` or `pub mod` line in
     that directory's mod.rs. Keep the module's public surface identical.
   - When a production file splits, its test file in
     crates/deepseek-custom-tests/tests/it/ splits the same way, following the naming rule
     in CLAUDE.md: the module path under crates/deepseek-custom/src/ with segments joined
     by underscores. Declare each new test module in tests/it/main.rs.
   - Replace an else branch with a guard clause or an early return.
   - Turn a duplicated block into one shared function or a helper, including in tests.
   - A stateless method becomes a free function.
   - An unnamed tuple becomes a named struct.
   - Never add a backward-compatibility shim, a re-export of a moved item, a feature flag,
     or a deprecated alias. Move the item and update every caller.
   - Delete dead exports the scanner names rather than making them public API.

5. Verify, in this order. Every command must pass before you check the box:

   cargo fmt --all
   cargo check --workspace
   cargo clippy --workspace -- -D warnings
   cargo test --workspace

   A failing test means the refactor changed behavior. Fix the code, not the test. Never
   delete, skip, or weaken a test to get green.

6. Rescan the file, and each new file the split produced. Every one must be at or under
   the bounds in step 4. If a file still breaks a rule, keep working rather than checking
   the box.

7. Update docs/notes/quality-checklist.md in two steps. First change that file's `- [ ]`
   to `- [x]` and append `- done: <one line saying what changed>` under it. Then rescan
   every crate and regenerate the whole checklist:

   node C:\Users\siriu\.claude\plugins\cache\dod-guard\quality-guard\037c77ae9669\skills\quality-refactor\scripts\quality-scan.mjs crates --root=. --format=units > .quality/units.json
   node scripts/quality-checklist.mjs .quality/units.json

   Regenerating carries your tick and your note forward, and it is how a new file the
   split produced enters the list, at its real score and in the right place. Do not add a
   box for a new file by hand. A hand-added row lands where you typed it rather than where
   its score puts it, and that is what used to leave the list out of order.

8. Update CLAUDE.md when the split changed the architecture a reader needs to know: a new
   module, a moved type, a changed public surface. Keep AGENTS.md in step with it.

9. Commit with a message shaped `refactor(<module>): <what moved where>`. Do not push.

Constraints for the whole iteration:
- Touch only what this one file's refactor forces you to touch: the file you picked, the
  files its split produced, the mod.rs that declares them, its tests and tests/it/main.rs,
  every call site an item's move breaks, the checklist, and the two guide documents. In a
  call site, change the import or the path and nothing else. Leave every other file alone.
- No em-dash or en-dash anywhere you write. ASCII punctuation only.
- If the file needs a design change rather than a structural one, say so in the checklist
  entry instead of checking the box, leave the code alone, and stop.
```
