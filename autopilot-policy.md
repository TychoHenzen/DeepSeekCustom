# Autopilot policy

This file answers every `AskUserQuestion` an autopilot run makes. No human
ever sees those questions. `PolicyStore::load_policy` reads this file fresh
on each question, so an edit here reaches the next question in a running
session.

## General

- Prefer the option that finishes the current checklist item. Do not pick an
  option that widens scope into a later item.
- Prefer the smaller change when two options both satisfy the item.
- Never pick an option that leaves the build broken or a test failing.
- Never pick an option that skips a verification step.
- When an option would delete or rewrite a file the current item does not
  name, pick a different option.

## Tests

- Always pick the option that adds a test over the option that does not.
- Tests belong in `crates/deepseek-custom-tests`. Never pick an option that
  puts a test in the production crate.
- Never pick an option that marks a test ignored, skipped, or expected to
  fail.
- When an option would make a production item `pub` only so a test can reach
  it, prefer an option that routes the test through an existing public seam,
  or one that puts the item behind the `test-support` feature.

## Dependencies

- Prefer an option that uses a crate the workspace already depends on.
- When an option adds a new dependency, pick it only if no already-present
  crate can do the job.

## Style

- Prefer plain ASCII punctuation. Never pick an option whose text carries an
  em dash, a curly quote, or an ellipsis character.
- Prefer the option with the shorter, commoner wording.
- Prefer active voice over passive.

## Structure

- Prefer an option that keeps a file under 300 lines and a function under 60.
- Prefer one type per file.
- Never pick an option that adds a backward-compatibility shim, a feature
  flag for an old path, or a fallback to a previous implementation. This is a
  solo project with no outside callers.
