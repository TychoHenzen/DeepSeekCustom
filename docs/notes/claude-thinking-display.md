# claude --thinking-display verification

Why a `claude_cli` turn used to show an empty "Reasoning" fold, and what
fixed it. Tested against the real binary on this machine, 2026-08-06.

```
$ claude --version
2.1.220 (Claude Code)
2.7.13 (tweakcc-fixed)
(patched)
```

## The symptom

An autopilot run on the `claude` backend saved four `Reasoning` spans, all
empty strings, in `.deepseek/sessions/ee93f3bf-....json`. Every DeepSeek
session in the same directory held reasoning text, so the gap was the
backend, not the tab.

## What the stream looked like

The harness's own flag set, minus `--thinking-display`, on `--model opus`:

```
content_block_start {"type":"thinking","thinking":"","signature":""}
thinking_delta      {"thinking":"","estimated_tokens":50}
thinking_delta      {"thinking":"","estimated_tokens":null}
signature_delta     {"signature":"CAIS9gEKhwEIEBgCKkARhtkkyAEMixEb..."}
```

The same run with `--thinking-display summarized` added:

```
thinking_delta {"thinking":"Classic","estimated_tokens":null}
thinking_delta {"thinking":". 0.05.","estimated_tokens":null}
```

So the text is available. It was never a server-side redaction, and adding
`--effort` or a "think hard" prompt made no difference either way.

## Why the flag is needed

The gate is on the request. The binary picks a thinking display mode from
this function, which defaults a non-interactive run to `omitted`:

```js
function uUc({explicitDisplay:e,isNonInteractive:t,outputFormat:r,verbose:n}){
  if(e)return e;
  if(!t)return C5i()?"summarized":void 0;
  if(r==="text"||r==="json"&&!n)return"omitted";
  return}
```

`omitted` asks the API for no thinking text at all, which is why the deltas
arrive with an empty field beside an encrypted signature. Nothing on the
parsing side can recover text that was never sent.

A patched binary does not change this. Both patches on this machine, the
`__cc_onStreamingThinking` hooks and tweakcc's `thinkingVisibility.ts`,
edit the interactive Ink component. Print mode never mounts it.

The flag is hidden from `--help` and takes two values, `summarized` or
`omitted`. It sits beside `--thinking`, which takes `enabled`, `adaptive`,
or `disabled`.

## What changed in the harness

`build_args` and `build_one_shot_args` both pass `--thinking-display
summarized` on every spawn. `EventMapper` still drops a `thinking_delta`
whose `thinking` field is empty. A redacted-thinking phase streams pings
with no text, and an empty fold promises content that does not exist.
