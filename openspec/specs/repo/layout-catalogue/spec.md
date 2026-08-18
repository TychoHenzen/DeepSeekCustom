# repo/layout-catalogue Specification

## Purpose
Records what every top-level entry in this repository is, what writes it, and whether git tracks it, and states the hygiene rules a new entry is judged against so that clutter is caught when it appears rather than surveyed again later.
## Requirements
### Requirement: The catalogue covers every top-level entry

The repository SHALL hold a catalogue document at `docs/notes/repo-layout.md`. It SHALL carry one row per entry at the top level of the repository, whether that entry is tracked, ignored, or untracked. Each row SHALL name the entry, what writes it, its git status, and its disposition, where a disposition is one of keep, ignore, delete, or decide.

#### Scenario: Every top-level entry has a row

- **WHEN** a reader lists the top level of the repository and compares that listing against the catalogue
- **THEN** every listed entry appears as exactly one catalogue row, and the catalogue names no entry that the listing does not hold

#### Scenario: A row names its writer and its disposition

- **WHEN** a reader opens any catalogue row
- **THEN** that row states what process or person creates the entry, whether git tracks it, and one of the four dispositions

### Requirement: A generated directory carries a naming ignore entry

Any directory that a tool, hook, skill, or workflow writes SHALL have an ignore rule, in this repository's `.gitignore` or in a nested `.gitignore`, and not in `.git/info/exclude`, which a clone does not carry. Any ignore rule this change adds or moves SHALL carry a comment naming what writes the entry. A rule that predates this change and lacks such a comment is recorded in the catalogue rather than rewritten, because renaming every existing writer is a separate job from cataloguing the layout.

#### Scenario: A generated directory is ignored and attributed

- **WHEN** the catalogue records an entry whose writer is a tool rather than a person
- **THEN** an ignore rule matches that entry, and a comment beside the rule names the writer

#### Scenario: A local-only exclusion is promoted

- **WHEN** an ignore rule for a generated directory lives only in `.git/info/exclude`, which a clone does not carry
- **THEN** the rule moves into the checked-in `.gitignore` with its naming comment

### Requirement: No unattributed artifact sits untracked in the root

The repository root SHALL hold no file that is neither tracked by git nor matched by a checked-in ignore rule. A machine-global ignore file SHALL NOT be relied on to hide such a file, because a clone on another machine does not carry it.

#### Scenario: A stray log is caught on a clean machine

- **WHEN** the repository is cloned onto a machine with no global ignore file and `git status --porcelain` runs
- **THEN** the output is empty

### Requirement: A checked-in configuration does not depend on an ignored file

A file that git tracks SHALL NOT reference a path that a checked-in ignore rule excludes. Either the referenced file becomes tracked, or the reference is removed.

#### Scenario: A hook script its config calls is reachable after a clone

- **WHEN** a tracked settings or hook file names a script path, and the repository is freshly cloned
- **THEN** that script exists in the clone

### Requirement: An empty tool directory is removed

The repository SHALL hold no top-level tool-configuration directory that carries no file at any depth and no ignore rule of its own. Such a directory has no consumer and no content, so it records only that a tool once ran. This says nothing about `.git/` or about a build directory such as `target/`, whose empty subdirectories are its own business.

#### Scenario: An empty agent-config directory is gone

- **WHEN** a reader searches the repository for empty directories, ignoring everything under `.git/` and under a build directory the catalogue marks keep
- **THEN** the search reports no top-level tool-configuration directory

### Requirement: Bulk local data is deleted only with approval

An entry the catalogue marks as user data or as expensive-to-regenerate output SHALL carry the disposition decide, and SHALL NOT be deleted by a cleanup pass without the repository owner saying so for that entry by name.

#### Scenario: Saved conversations survive a cleanup pass

- **WHEN** a cleanup pass runs over the catalogue and reaches an entry whose disposition is decide
- **THEN** the pass leaves the entry in place and reports it as awaiting a decision

