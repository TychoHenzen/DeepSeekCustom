# claude --resume verification

Tested against the real `claude` binary on this machine.

```
$ claude --version
2.1.220 (Claude Code)
```

Resume works with the harness's exact flag combination. No blocker found.

## 1. Does the stream-json output carry a session id?

Yes. The `{"type":"system","subtype":"init",...}` event carries `session_id`.
The `{"type":"result",...}` event at the end of the turn carries the same
field.

Command:

```
echo '{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Remember the number 8471. Reply with just OK."}]}}' \
  | claude -p --output-format stream-json --input-format stream-json \
      --include-partial-messages --verbose --permission-mode bypassPermissions
```

Decisive line (init event, trimmed):

```
{"type":"system","subtype":"init","cwd":"...","session_id":"c18eb67f-6873-45a4-aa7a-8755cecb4361", ...}
```

Decisive line (result event, trimmed):

```
{"is_error":false, ..., "session_id":"c18eb67f-6873-45a4-aa7a-8755cecb4361", ..., "result":"OK", "type":"result", ...}
```

JSON path: top-level `session_id` field, present on both the `init` event
(first event of the stream) and the `result` event (last event of the
stream). It is a UUID string.

## 2. Does `--resume <session-id>` exist, and what is its exact spelling?

Yes. From `claude --help`:

```
  -r, --resume [value]                  Resume a conversation by session ID, or
                                        open interactive picker with optional
                                        search term
```

Exact spelling: `--resume` (long form), `-r` (short form). Takes the session
id as its value: `--resume <session-id>`.

`--continue` also exists, separately:

```
  -c, --continue                        Continue the most recent conversation in
                                        the current directory
```

`--continue` takes no id argument. It picks the most recent conversation for
the current working directory automatically. `--resume` takes an explicit id
and is not tied to guessing "most recent". For a harness that persists its
own session id per project, `--resume <id>` is the correct one, not
`--continue`.

A related flag also showed up in `--help` and is worth recording since it
touches the same id:

```
  --fork-session                        When resuming, create a new session ID
                                        instead of reusing the original (use
                                        with --resume or --continue)
```

Not used in this test. Noted for completeness: `--fork-session` is what you
would add if you wanted a resume to branch into a new id instead of
continuing the same one.

## 3. Does `--resume` work with the harness's exact flag set?

Yes, tested and confirmed. Two separate `claude` processes were run in
sequence with a closed stdin pipe (Git Bash on Windows).

First process, giving the model a fact to remember:

```
echo '{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Remember the number 8471. Reply with just OK."}]}}' \
  | claude -p --output-format stream-json --input-format stream-json \
      --include-partial-messages --verbose --permission-mode bypassPermissions
```

Result: `"result":"OK"`, `"session_id":"c18eb67f-6873-45a4-aa7a-8755cecb4361"`.

Second process, same flags plus `--resume <that id>`, asking for the fact
back:

```
echo '{"type":"user","message":{"role":"user","content":[{"type":"text","text":"What number did I tell you to remember? Reply with just the number."}]}}' \
  | claude -p --output-format stream-json --input-format stream-json \
      --include-partial-messages --verbose --permission-mode bypassPermissions \
      --resume c18eb67f-6873-45a4-aa7a-8755cecb4361
```

Decisive line:

```
{"is_error":false, ..., "session_id":"c18eb67f-6873-45a4-aa7a-8755cecb4361", ..., "result":"8471", "type":"result", ...}
```

The answer came back correct: `8471`. The second process also spent
`"cache_read_input_tokens":39492` against `"cache_creation_input_tokens":2068`,
confirming it picked up the first process's actual context rather than
starting cold. `--resume` is fully compatible with
`--output-format stream-json --input-format stream-json
--include-partial-messages --verbose`, plus `-p` and `--permission-mode
bypassPermissions`. No flag conflict, no error, no degraded behavior
observed.

## 4. Does the resumed process report the same session id, or a new one?

Same id. Both the `init` event and the `result` event of the second process
carry `session_id":"c18eb67f-6873-45a4-aa7a-8755cecb4361"`, identical to the
first process's id. Resuming does not mint a new id under plain `--resume`.
(`--fork-session`, noted above but not tested, is documented to change this
behavior on purpose. Left untested since the harness does not need it.)

This means the harness does not need to update its stored session id after
an ordinary resume. The id it wrote after the first turn stays valid for
every following resume. Only `--fork-session` changes that.

## 5. Incompatibilities, errors, or surprises

None that blocked the test. Two side notes:

- The init event on this machine carries a lot of setup detail beyond cwd,
  tools, and session_id. It lists installed plugins, skills, agents, and
  MCP servers. This account has a large global `~/.claude` config, which
  is why. S07 only needs the `session_id` field out of it. The rest is safe to
  ignore. Parsing should not assume the object is small.
- Both runs cost real money. The `total_cost_usd` field in the result event
  read about $0.40 for the first turn and about $0.04 for the resumed one.
  A large cached system prompt drove that, from this account's global
  CLAUDE.md and plugin set. A minimal test project would cost much less per
  run. Worth knowing before scripting repeated resume tests in CI.

## What S07 must do

- Flag to add to `build_args` in `src/backend/claude_cli/process.rs`: `--resume <session-id>`, positioned alongside the other flags, value taken from the harness's stored session id for that conversation.
- JSON path to parse the session id from: the top-level `session_id` field of the first stream event, `{"type":"system","subtype":"init",...}`. The terminal `{"type":"result",...}` event carries it too, so either point can supply it. Take it from the init event. That is the earliest it is available, and it matches how `EventMapper` already reads events in order.
- Whether the id changes on resume: no, confirmed by direct test. The first turn's id is the id reported on every later resumed turn. So the harness can store the id once, after the first `init` event, and reuse it. It never needs to re-read it. Only `--fork-session` yields a new id, and the harness does not pass it.
