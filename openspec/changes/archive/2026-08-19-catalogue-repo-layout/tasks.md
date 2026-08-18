<!-- plan_artifacts: [{"id":"proposal","outputPath":"proposal.md","status":"done","requires":[]},{"id":"specs","outputPath":"specs/**/*.md","status":"done","requires":["proposal"]},{"id":"design","outputPath":"design.md","status":"done","requires":["proposal"]},{"id":"tasks","outputPath":"tasks.md","status":"done","requires":["specs","design"]}] -->
## 1. Write the catalogue

- [x] 1.1 Create `docs/notes/repo-layout.md` with a header saying it is hand written, and a table whose columns are entry, writer, git status, and disposition. Fill one row per top-level entry, taking the survey in design.md - Context as the starting evidence and re-running `ls -a`, `git ls-files`, and `git check-ignore -v` to confirm each row rather than copying.
  <!-- covers: repo/layout-catalogue :: The catalogue covers every top-level entry :: Every top-level entry has a row -->
  <!-- status: completed -->
- [x] 1.2 Confirm every row names what writes the entry and carries one of keep, ignore, delete, or decide, with no cell left blank.
  <!-- covers: repo/layout-catalogue :: The catalogue covers every top-level entry :: A row names its writer and its disposition -->
  <!-- status: completed -->
- [x] 1.3 Diff the catalogue's entry column against `ls -a` output and confirm the two sets match exactly, in both directions.
  <!-- covers: repo/layout-catalogue :: The catalogue covers every top-level entry :: Every top-level entry has a row -->
  <!-- status: completed -->

## 2. Fix the ignore rules

- [x] 2.1 Move the `.evo/` exclusion out of `.git/info/exclude` and into `.gitignore`, with a comment naming gitevo's `evo_init` as its writer. Confirm `git check-ignore -v .evo/memory.db` then attributes the match to `.gitignore`.
  <!-- covers: repo/layout-catalogue :: A generated directory carries a naming ignore entry :: A local-only exclusion is promoted -->
  <!-- status: completed -->
- [x] 2.2 Add a `.gitignore` entry with a naming comment for every generated directory the catalogue marks as tool written and that no checked-in rule already matches. As of the survey that is `.code-review-graph/`, which is covered by its own nested `.gitignore`, so confirm rather than assume. Record any pre-existing uncommented rule (`/target`, `.idea/`, `.claude/`) as a catalogue row instead of rewriting it.
  <!-- covers: repo/layout-catalogue :: A generated directory carries a naming ignore entry :: A generated directory is ignored and attributed -->
  <!-- status: completed -->
- [x] 2.3 Add checked-in ignore patterns covering future stray root logs, `hs_err_pid*.log` for the JVM crash dumps and a pattern matching the mutants run log, each with a naming comment. Run this before task 3.2 deletes the current files, or the check passes on an empty root and proves nothing. Confirm with `git check-ignore -v hs_err_pid50096.log` that the reported source is `.gitignore` and no longer `gitignore_global.txt`.
  <!-- covers: repo/layout-catalogue :: No unattributed artifact sits untracked in the root :: A stray log is caught on a clean machine -->
  <!-- status: completed -->

## 3. Remove the dead entries

- [x] 3.1 For each of `.clinerules/`, `.cursor/rules/`, `.opencode/`, and `.windsurf/rules/`, confirm it holds zero files at any depth with `find <dir> -type f`, then delete it. Delete `.cursor/` and `.windsurf/` too if removing the inner directory leaves them empty.
  <!-- covers: repo/layout-catalogue :: An empty tool directory is removed :: An empty agent-config directory is gone -->
  <!-- status: completed -->
- [x] 3.2 Delete the two current stray logs, `hs_err_pid50096.log` and `mutants_scoped_run.log`, from the root. Task 2.3 added the patterns; this task removes the instances. Both are one-off crash and run output, dated 2026-05-29 and 2026-08-05, and neither is referenced by any tracked file.
  <!-- covers: repo/layout-catalogue :: No unattributed artifact sits untracked in the root :: A stray log is caught on a clean machine -->
  <!-- status: completed -->
- [x] 3.3 Re-run `find . -type d -empty -not -path './.git/*' -not -path './target/*'` and confirm it reports no top-level tool-configuration directory. The two exclusions match the scenario, which exempts `.git/` and a build directory the catalogue marks keep.
  <!-- covers: repo/layout-catalogue :: An empty tool directory is removed :: An empty agent-config directory is gone -->
  <!-- status: completed -->

## 4. Repair the tracked hook dependency

- [x] 4.1 Remove the `time.ps1` line from `.gitignore` and run `git add time.ps1`, so the script both tracked configurations call ships with a clone.
  <!-- covers: repo/layout-catalogue :: A checked-in configuration does not depend on an ignored file :: A hook script its config calls is reachable after a clone -->
  <!-- status: completed -->
- [x] 4.2 Grep every tracked file for paths that a checked-in ignore rule excludes, and record each remaining case in the catalogue. Fix any whose fix is as small as 4.1; leave the rest as catalogue rows naming the problem.
  <!-- covers: repo/layout-catalogue :: A checked-in configuration does not depend on an ignored file :: A hook script its config calls is reachable after a clone -->
  <!-- status: completed -->

## 5. Ask about the large entries

- [x] 5.1 Mark `.deepseek/sessions/` (28 MB of saved conversations), `mutants.out/` (19 MB), and `.code-review-graph/graph.db` (5.9 MB) with the `decide` disposition in the catalogue, each with a one-line note on what deleting it would cost.
  <!-- covers: repo/layout-catalogue :: Bulk local data is deleted only with approval :: Saved conversations survive a cleanup pass -->
  <!-- status: completed -->
- [x] 5.2 Ask the repository owner about each of the three by name, and record the answer in the catalogue row. Delete nothing until an answer names that entry.
  <!-- covers: repo/layout-catalogue :: Bulk local data is deleted only with approval :: Saved conversations survive a cleanup pass -->
  <!-- status: completed -->

## 6. Close out

- [x] 6.1 Update CLAUDE.md and AGENTS.md together if any task above moved or removed a path either document names. CLAUDE.md requires the two be edited as a pair.
  <!-- status: completed -->
- [x] 6.2 Run `cargo test --workspace` and `cargo clippy --workspace -- -D warnings` as a regression check. No Rust source changed, so both are expected to pass unchanged.
  <!-- status: completed -->
- [x] 6.3 Run `git status --porcelain` and account for every line before committing, per the CLAUDE.md rule on `git add -A`.
  <!-- status: completed -->
