# claude --effort verification

Tested against the real `claude` binary on this machine.

```
$ claude --version
2.1.220 (Claude Code)
2.7.13 (tweakcc-fixed)
(patched)
```

Note on version: this binary reports three version lines, not the single
line `docs/notes/claude-resume.md` recorded. The middle line names a
"tweakcc-fixed" build and the third says "(patched)". This machine's `claude`
is not a stock install. Worth knowing if a mapping written from this test
ever looks wrong on a different machine's install.

A dedicated flag exists and was confirmed accepted with the harness's exact
flag set. No blocker found.

## 1. Does `claude --help` list an effort flag?

Yes.

```
  --effort <level>                      Effort level for the current session
                                        (low, medium, high, xhigh, max)
```

Five levels: `low`, `medium`, `high`, `xhigh`, `max`. This is a different
level count than either DeepSeek (`thinking`, `thinking_max`, `non-thinking`,
three levels) or Ollama's `reasoning_effort` (`max`, `high`, `medium`, `low`,
`none`, five levels but not the same names: Ollama has `none`, this flag has
`xhigh` in the same slot count).

## 2. Does the flag reject an invalid value, and what does that reveal?

Yes, and the rejection message is more informative than the `--help` text
alone. Command, with no real turn sent (empty stdin, so the query fails
before any model call is billed):

```
$ echo '' | claude -p --effort bogus
Warning: Unknown --effort value 'bogus' — ignoring it and using the default
effort. Valid values: low, medium, high, xhigh, max.
Error: No messages returned from query
```

This confirms the accepted value set directly from the binary's own
validation code, not just from `--help` prose: `low`, `medium`, `high`,
`xhigh`, `max`. An unrecognized value does not error out the whole process,
it falls back to the default effort and continues. That fallback still cost
nothing here since the run then failed for the unrelated reason of empty
input.

## 3. Does `--effort` work with the harness's exact flag set?

Yes, tested and confirmed. Full command, matching `src/backend/claude_cli/process.rs`'s
`-p --output-format stream-json --input-format stream-json
--include-partial-messages --verbose --permission-mode bypassPermissions`,
plus `--effort low`:

```
$ echo '{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Reply with just OK."}]}}' \
  | claude -p --output-format stream-json --input-format stream-json \
      --include-partial-messages --verbose --permission-mode bypassPermissions \
      --effort low
```

Result event (trimmed):

```
{"is_error":false, ..., "session_id":"54c94196-c54a-48c1-a011-1fe7a8b88ee5",
 "total_cost_usd":0.686006, ..., "result":"OK", "type":"result", ...}
```

No error, no warning, no flag-conflict message anywhere in the 29 lines of
output. `is_error` is `false` and the turn completed normally. `--effort` is
compatible with `--output-format stream-json --input-format stream-json
--include-partial-messages --verbose`, plus `-p` and `--permission-mode
bypassPermissions`.

This run did not pass `--model`, so it ran on this account's default model,
`claude-opus-5` per the `init` event, which is why the cost came out high
for a two-word reply (`total_cost_usd: 0.686006`, most of it a 68465-token
cache-creation charge on `claude-opus-5`, plus a small `claude-haiku-4-5`
charge from a background task). That cost is a property of the default
model and cache state on this account, not of the `--effort` flag itself.

## 4. Does the stream-json output surface which effort level was actually used?

No, or at least not observed. Neither the `init` event nor the `result`
event exposes an `effort` field anywhere in the captured output. Searching
the full 29-line capture for the string "effort" turns up exactly one match,
and it is unrelated: a slash-command name list embedded in a
`SessionStart:startup` hook's own output text (`"...,\"context\",\"effort\",\"fast\",...\"`),
not a field describing the turn's reasoning effort. So this test confirms
the flag is accepted and the turn completes normally. It does not confirm,
from the wire protocol alone, that the requested level actually changed
model behavior. That would need either a documented field this harness has
not found yet, or an indirect signal (token usage or latency compared across
levels on the same prompt), and neither was tested here.

## 5. Incompatibilities, errors, or surprises

None that blocked the test. Two notes:

- This account's `claude` binary is a modified build (see the three-line
  version output above), not the plain 2.1.220 the `--resume` note tested
  against. The `--effort` flag itself is unlikely to be a local patch, since
  it is a normal-looking session-level option next to `--model` and
  `--permission-mode` in `--help`, but this was not independently confirmed
  against a stock install.
- The one real turn run here cost about $0.69, driven by the account's
  default model (`claude-opus-5`) and a large cache-creation charge from
  this account's global config, the same effect the `--resume` note already
  recorded for its own test turns. A minimal test project or an explicit
  cheaper `--model` would cost much less per run.

## What the next step must honour

- Flag to add to `build_args` in `src/backend/claude_cli/process.rs`:
  `--effort <level>`, value one of exactly `low`, `medium`, `high`, `xhigh`,
  `max`. This is a five-level enum, not three and not the same five names as
  Ollama's `reasoning_effort`.
- The harness's `Effort` enum from the roadmap (`None, Low, Medium, High,
  Max`) has no direct `None` counterpart on this path: `--effort` offers no
  `none` value, only `low` as its lowest level. A mapping from `Effort::None`
  has to either omit `--effort` entirely (leaving the CLI's own default) or
  fall back to `low`, since there is nothing lower to send. That choice is
  unresolved here, on purpose, since writing the mapping is out of scope for
  this step.
- `Effort::Max` maps to `--effort max` directly. Nothing was found that
  distinguishes a `Max` mapping from `xhigh`, since the harness's enum does
  not have a slot between `High` and `Max` the way the CLI's `xhigh` sits
  between `high` and `max`. That is also left for the mapping step to
  decide, not guessed here.
- No field in the stream-json protocol was found that reports back which
  effort level a turn actually ran at. A mapping step should not assume one
  exists without checking further, and should not build any logic that reads
  effort back out of the response.
