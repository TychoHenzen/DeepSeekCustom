## Context

See proposal.md for motivation. What follows is the survey this change rests on, run on 2026-08-18 against the repository at commit `2288d12`, working tree clean.

The top level holds 34 entries. 15 of them are tracked by git, and `git ls-files` counts 233 tracked files under `crates/`, 17 under `docs/`, 6 under `openspec/`, and 5 under `scripts/`. The rest is a mix of workflow scratch, tool output, and empty directories.

Four findings shaped the requirements in `specs/repo/layout-catalogue/spec.md`.

1. `.clinerules/`, `.cursor/rules/`, `.opencode/`, and `.windsurf/rules/` each hold zero files. `git status` reports the tree clean because git tracks files, not directories, so these are invisible to every check that reads git alone. They record only that four editors were once pointed at this repository.
2. `hs_err_pid50096.log`, `mutants_scoped_run.log`, and `deepseek_custom.log` sit untracked in the root. `git check-ignore` attributes the first two to line 21 of `C:\Users\siriu\OneDrive\Documenten\gitignore_global.txt`, a machine-global file no clone carries. On another machine all three show as `??`.
3. `time.ps1` is excluded by `.gitignore`, and both `.claude/settings.json` and `.codex/hooks.json` call it by absolute path as a `PreToolUse` hook. A fresh clone gets the hook configuration in `.codex/hooks.json`, which is tracked, and not the script it runs.
4. `.evo/` is excluded by `.git/info/exclude` rather than by the checked-in `.gitignore`. That exclusion is local to this clone. `.code-review-graph/` is handled correctly by contrast: it carries its own nested `.gitignore` holding `*`.

Three entries are large and cannot be judged on hygiene alone: `.deepseek/sessions/` at 28 MB of saved conversations, `mutants.out/` at 19 MB, and `.code-review-graph/graph.db` at 5.9 MB. `target/` is 2.4 GB and is already handled by the post-commit hook `scripts/hooks/post-commit`.

## Goals / Non-Goals

**Goals:**

- One document a later cleanup pass reads instead of re-running this survey.
- Rules stated once, so a new top-level entry is judged rather than argued about.
- Every fix in this change is checkable from a shell, with no judgment call left in the verification.

**Non-Goals:**

- Anything under `crates/`. See proposal.md - Impact.
- Deciding the fate of the three large data entries. This change records them and asks; it does not delete them.
- Auditing whether the plans and notes under `docs/` are still accurate. That is a content question, and this change is about layout.
- Adding a script or CI job that enforces the rules. The rules are checked by hand for now, and automating them is a later change.

## Decisions

**The catalogue lives at `docs/notes/repo-layout.md`.** `docs/notes/` already holds the generated `quality-checklist.md` and six investigation notes, so a reader looking for repository facts already goes there. The alternative, a root-level `LAYOUT.md`, adds a root entry to a change whose whole subject is that the root holds too many entries.

**The catalogue is written by hand, not generated.** The disposition column carries judgment that no script can supply: `mutants.out` and `.deepseek/sessions` are both large untracked directories and they get different answers. A generated table would have to leave that column empty, which is the only column worth having. This is the opposite of `docs/notes/quality-checklist.md`, which is generated because its ranking is mechanical.

**`time.ps1` becomes tracked.** Two tracked configuration files call it, so removing the reference would mean editing a hook out of `.codex/hooks.json` that the user actively uses. The file is 1 KB of PowerShell that prints the current time. Tracking it costs nothing and makes a clone work. The rejected alternative, dropping the hook, changes the user's tooling to satisfy a hygiene rule.

**`.evo/` moves from `.git/info/exclude` into `.gitignore`.** The exclusion is correct and its home is not: `.git/info/exclude` is per clone, so the next clone sees `.evo/lessons.jsonl` and `.evo/memory.db` as untracked. Moving it also puts it beside the naming comments every other generated directory already carries in `.gitignore`.

**Empty tool directories are deleted rather than given a `.gitkeep`.** A `.gitkeep` would make `.clinerules/` tracked and permanent, which asserts that this repository supports Cline. Nothing else in the repository says that. Deleting is reversible: the editor recreates its directory the next time someone points it here.

**The three large entries get the `decide` disposition and one question each in tasks.md.** They are not deleted in this change. `.deepseek/sessions/` is user data by CLAUDE.md's own description, `mutants.out` cost 5 hours of compute by the estimate in `docs/notes/mutants.md`, and `graph.db` is an MCP server's index.

## Risks / Trade-offs

- A hand-written catalogue goes stale the first time someone adds a root entry without updating it. -> The rules requirement is what carries the weight: the catalogue is the evidence, and a stale row is a smaller loss than no stated rule. A later change can add an enforcing check.
- Deleting an empty editor directory could remove a configuration the user meant to fill in later. -> Each of the four is confirmed to hold zero files at any depth before deletion, and git can restore nothing that git never held, so the check runs before the delete rather than after.
- Tracking `time.ps1` checks in an absolute Windows path's dependency. -> The script itself carries no path. The absolute paths sit in `.claude/settings.json`, which `.gitignore` already excludes, and in `.codex/hooks.json`, which is tracked and already carries one. That is a separate problem this change records in the catalogue rather than fixes.
- The catalogue and CLAUDE.md can disagree about what a directory is for. -> The tasks that touch a documented path update CLAUDE.md and AGENTS.md together, which CLAUDE.md already requires.

## Open Questions

- Whether the four empty editor directories should be replaced by a single note in the catalogue saying this repository standardises on Claude Code and Codex. That is a content decision and it changes no task here.
