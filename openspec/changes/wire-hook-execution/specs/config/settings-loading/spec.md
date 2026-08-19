## Purpose

Defines how the harness finds, parses, and merges its settings files, and what it does when one cannot be read. Exists because a file that fails to parse currently disappears without a word, which turns one bad block into the silent loss of every setting that file carried.

## ADDED Requirements

### Requirement: Settings load from a fixed precedence chain

Settings SHALL be read from the project's own settings file, then the user's global Claude Code settings, then the user's global harness settings, with a later file overriding an earlier one field by field.

A file that does not exist SHALL NOT be an error. Its absence SHALL leave the values from earlier files, or the defaults, in place.

#### Scenario: A later file overrides an earlier one

- **WHEN** two files in the chain both set one field
- **THEN** the value from the later file wins, and fields only the earlier file sets are kept

#### Scenario: A missing file is not an error

- **WHEN** one or more files in the chain do not exist
- **THEN** loading succeeds using whatever files do exist, and the defaults for anything none of them set

### Requirement: An unreadable settings file is reported, never silently dropped

A settings file that exists but cannot be parsed SHALL be reported where a user can see it. The report SHALL name the file's path and what was wrong with it.

Loading SHALL continue with the other files in the chain, so one unreadable file does not stop the harness from starting.

#### Scenario: A malformed file names itself

- **WHEN** a settings file in the chain holds JSON that does not parse
- **THEN** the failure is reported naming that file's path and the parse error, and the remaining files still load

#### Scenario: A file with one bad block does not void its other settings

- **WHEN** a settings file holds one block the harness cannot read alongside blocks it can
- **THEN** the readable blocks still take effect, and the unreadable block is reported naming what could not be read

#### Scenario: Startup survives an unreadable file

- **WHEN** every file in the chain fails to parse
- **THEN** the harness starts on defaults and reports each failure, rather than failing to start or starting silently

### Requirement: An unknown field does not fail a load

A settings file holding a field the harness does not recognize SHALL still load. The unrecognized field SHALL be ignored.

This is what lets one settings file serve both this harness and Claude Code, and lets a file written by a later version load in an earlier one.

#### Scenario: A foreign field is ignored

- **WHEN** a settings file holds fields this harness does not know
- **THEN** the file loads, the known fields take effect, and the unknown fields are ignored without a failure
