# Image support per backend, checked against real requests

## Web attachment limits

The local web app accepts one PNG, JPEG, or BMP image per turn. Paste, drop,
and file selection use the same multipart endpoint. The decoded upload must be
between 1 byte and 5 MiB. A new image replaces the pending preview. Submission
consumes its attachment identity, so it cannot be reused by a later turn.

Tested against the live DeepSeek API, a local Ollama 0.32.5 instance, and the
real `claude` binary on this machine (`2.1.220 (Claude Code)`, the same
"tweakcc-fixed, patched" build `docs/notes/claude-effort.md` already noted).
Every claim below comes from a request that was actually sent and a response
that was actually read. Nothing here is guessed.

The test image is a real 68-byte PNG, not a placeholder string. Decoding its
header shows `bit depth 08, color type 04`, grayscale with alpha, a single
gray pixel, not the red pixel a copy-pasted "1x1 red PNG" snippet usually is.
That mismatch turned out useful: a model that reports "gray" back is proving
it decoded the actual bytes, not pattern-matching a well-known test fixture.

```
$ node -e "console.log(Buffer.from('<base64 below>','base64').toString('hex'))"
89504e470d0a1a0a0000000d4948445200000001000000010804000000b51c0c...
```

The base64 payload used in every request below:

```
iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=
```

## 1. DeepSeek: does the live API accept an `image_url` content part?

No. The live API rejects the whole request at the schema level before any
model runs.

Model: `deepseek-v4-flash`, the current `deepseek` entry's model in
`settings.json`. Key resolved from `~/.claude/backends.json`, the
`deepseek-home` entry (the file's `default`), through the same
`DEEPSEEK_API_KEY` env -> ... -> `~/.claude/backends.json` chain
`src/api/client.rs`'s `resolve_api_key` documents. No `DEEPSEEK_API_KEY` or
`ANTHROPIC_AUTH_TOKEN` env var was set on this machine; the key came from
that last link in the chain.

Request, the exact `ContentPart::ImageUrl` wire shape from `src/api/types.rs`:

```
$ curl -s -X POST "https://api.deepseek.com/chat/completions" \
  -H "Authorization: Bearer $DEEPSEEK_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
  "model": "deepseek-v4-flash",
  "messages": [
    {
      "role": "user",
      "content": [
        {"type": "text", "text": "What color is this image? Answer in one word."},
        {"type": "image_url", "image_url": {"url": "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII="}}
      ]
    }
  ],
  "stream": false
}'
```

Response:

```
{"error":{"message":"Failed to deserialize the JSON body into the target
type: messages[0]: unknown variant `image_url`, expected `text` at line 1
column 357","type":"invalid_request_error","param":null,"code":"invalid_request_error"}}
```

That message names `messages[0]` and says the only accepted content-part
variant is `text`. This is a deserialization error on the server's own
request type, not a model-level refusal, so it is very unlikely to be
specific to `deepseek-v4-flash`. Only that one model was tried; `deepseek-v4-pro`
was not, on the grounds that a schema-level "unknown variant" rejection is a
property of the endpoint's parser, not of which model name follows it. If a
later check needs certainty on `-pro` too, that is a five-second follow-up,
not tested here.

**Conclusion:** DeepSeek's `chat/completions` endpoint, at least as this
account's plan currently exposes it, does not accept an `image_url` content
part at all. Sending one is not a soft failure or a "model can't see it"
answer, it is a hard 400 that kills the whole request before the model ever
runs. A mapping for this backend needs to either drop the image part before
it reaches `ApiClient` (with a notice in the transcript, per the roadmap's
own instruction for a backend that can't take an image) or reject the turn
early with a clear error, never send an `image_url` part to this endpoint.

## 2. Ollama: vision model vs non-vision model, on this machine's real instance

Half-confirmed. The non-vision case is confirmed. The vision case could not
be tested: no vision-capable model is installed on this machine's Ollama.

```
$ curl -s http://localhost:11434/api/tags
```

Two models are present: `qwen2.5:1.5b` and `qwen2.5-coder:7b-instruct-q4_K_M`.
Both list `"capabilities":["completion","tools"]` or
`["completion","tools","insert"]`, neither lists `vision`. `settings.json`'s
`ollama` backend entry points at `qwen2.5-coder:7b-instruct-q4_K_M`, the
second of those two, so the backend this harness actually runs today has no
vision model behind it either. Pulling a vision model (`llava`, `moondream`,
or similar) to complete the other half of this question was not done: the
brief says to check what is actually installed rather than assume, and says
plainly if none is present, which is the case here. Pulling a new multi-GB
model was judged out of scope for "keep it small."

Request against the installed non-vision model, same OpenAI-compatible
`image_url` shape as the DeepSeek test:

```
$ curl -s -X POST "http://localhost:11434/v1/chat/completions" \
  -H "Authorization: Bearer ollama" \
  -H "Content-Type: application/json" \
  -d '{
  "model": "qwen2.5-coder:7b-instruct-q4_K_M",
  "messages": [
    {
      "role": "user",
      "content": [
        {"type": "text", "text": "What color is this image? Answer in one word."},
        {"type": "image_url", "image_url": {"url": "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII="}}
      ]
    }
  ],
  "stream": false
}'
```

Response:

```
{"error":{"message":"{\"error\":{\"code\":400,\"message\":\"Multimodal data
provided, but model does not support multimodal requests.\",\"type\":\"invalid_request_error\"}}",
"type":"invalid_request_error","param":null,"code":null}}
```

Ollama itself recognizes the `image_url` part (unlike DeepSeek, this is not
a schema rejection naming an unknown variant) and answers with a named,
specific error about the model, not the request shape: the part is
well-formed, this particular model just can't take it.

**Conclusion, non-vision half:** confirmed. A non-vision Ollama model gives a
clean, named 400 that a mapping can detect and turn into the transcript
notice the roadmap asks for, distinct from a malformed-request error.
**Conclusion, vision half:** not tested on this machine, for the plain
reason that no vision model is installed. Nothing here should be taken as a
finding about how a vision model on Ollama actually behaves; that needs a
model pull first, on a future machine or after fetching one, and a separate
check.

## 3. claude_cli: can the stdin turn shape carry an image at all?

Yes. The roadmap's guess that this path "looks unable" to carry an image is
wrong. The turn's content array accepts a `type: "image"` content block,
Anthropic's own content-block shape, not the OpenAI-compatible `image_url`
shape the other two backends speak, and the turn completes normally.

Command, matching `src/backend/claude_cli/process.rs`'s `build_args` flag
set exactly (`-p --output-format stream-json --input-format stream-json
--include-partial-messages --verbose --model haiku --permission-mode
bypassPermissions`), one stdin line:

```
$ echo '{"type":"user","message":{"role":"user","content":[{"type":"text","text":"What color is this image? Answer in one word."},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII="}}]}}' \
  | claude -p --output-format stream-json --input-format stream-json \
      --include-partial-messages --verbose --model haiku \
      --permission-mode bypassPermissions
```

`--model haiku` was passed on purpose, to keep the cost of confirming this
down; the default account model is `claude-opus-5` per the `--effort` note's
earlier test, and that model costs far more per turn.

Result event (trimmed):

```
{"is_error":false,"stop_reason":"end_turn","session_id":"dffb4246-96ac-4895-a969-c7d72a96965d",
"total_cost_usd":0.10254300000000001,"usage":{"input_tokens":10,
"cache_creation_input_tokens":50499,"cache_read_input_tokens":0,"output_tokens":307,...},
"modelUsage":{"claude-haiku-4-5-20251001":{...,"canonicalModel":"claude-haiku-4-5",...}},
"result":"Gray.","type":"result",...}
```

`is_error` is `false`. The model answered `"Gray."`, and the pixel really is
a grayscale pixel, confirmed above by decoding the PNG header independently
of anything the model said. That is not a lucky guess: nothing about the
prompt text told the model the image was grayscale rather than colored, and
a well-known "1x1 red pixel" test snippet would have made a wrong "gray"
answer suspicious in the other direction. Getting the actual pixel type
right is direct evidence that the bytes reached the model and were decoded
as an image, not silently dropped or ignored. Searching the full 68-line
captured output for "cannot", "unable", "don't see", and "no image" found
two unrelated hits, both inside an unrelated CLAUDE.md/hook excerpt about a
`DROP TABLE` warning that got pulled into context, not anything about the
image.

Cost: this one real turn cost $0.1025, on `claude-haiku-4-5-20251001`, driven
mostly by a 50,499-token cache-creation charge from this project's own
CLAUDE.md and hook context, the same effect `docs/notes/claude-effort.md`
already recorded for its own test turn on this account.

**Conclusion:** the stdin turn shape can carry an image today, but not
through the harness's own `ContentPart::ImageUrl`, which serializes to the
OpenAI `{"type":"image_url","image_url":{"url":...}}` shape. This path needs
its own, different content-block shape when the mapping is written:
`{"type":"image","source":{"type":"base64","media_type":"<mime>","data":"<base64, no data: prefix>"}}`.
That is a real difference from the other two backends' wire shape, not a
detail the mapping step can share across all three. The roadmap's fallback
plan, writing the image to a temp file and naming it by path in the prompt
text, is not needed for this path: the protocol already has a direct way to
carry the bytes inline.

## Summary table for the mapping step

| Backend | Accepts an image on the wire? | Shape needed | Confirmed how |
|---|---|---|---|
| DeepSeek (`deepseek-v4-flash`) | No, hard 400 naming `image_url` as an unknown content-part variant | N/A, backend cannot take one | Real API call, real key |
| Ollama, non-vision model (`qwen2.5-coder:7b-instruct-q4_K_M`, this machine's `ollama` entry) | No, named 400 "model does not support multimodal requests" | N/A for this model | Real local call |
| Ollama, vision model | Not tested, none installed on this machine | Unknown, needs a vision model pulled first | Not tested, said plainly |
| `claude_cli` | Yes, turn completes and answers correctly about the real image | Anthropic content block: `{"type":"image","source":{"type":"base64","media_type":"...","data":"..."}}`, not the OpenAI `image_url` shape this harness's `ContentPart` produces | Real `claude -p` turn, cost $0.1025 |

Where a backend cannot take an image (DeepSeek, and any Ollama model without
vision, which today is every model actually installed here), the harness
should say so in the transcript rather than dropping the attachment
silently, exactly as the roadmap already says. This note does not write that
logic; it only establishes what each backend actually does, for the mapping
step to build against.

## Follow-up: the claimed /v1/vision endpoint

A third-party blog post claims DeepSeek accepts images at
`https://api.deepseek.com/v1/vision` on a model named `deepseek-vision`.
That claim is wrong. Checked directly.

The blog cites no DeepSeek documentation. Its own title says "Potential".

With a valid key, so the request actually routes:

```
$ curl -X POST https://api.deepseek.com/v1/vision -H "Authorization: Bearer $KEY" ...
HTTP 404

$ curl -X POST https://api.deepseek.com/v1/vision/completions ...
HTTP 404

$ curl https://api.deepseek.com/v1/models -H "Authorization: Bearer $KEY"
{"object":"list","data":[{"id":"deepseek-v4-flash",...},{"id":"deepseek-v4-pro",...}]}
```

There is no `deepseek-vision` model. There are two models and neither takes
an image. The earlier test used `deepseek-v4-flash`. `deepseek-v4-pro`
rejects an image the same way:

```
{"error":{"message":"Failed to deserialize the JSON body into the target type:
messages[0]: unknown variant `image_url`, expected `text` ...
```

One trap worth naming, because it looks like evidence and is not. An
unauthenticated GET to `/v1/vision` answers `Authentication Fails
(governor)` rather than a 404, which reads as though the path exists. It
does not. That response is path-independent:

```
/v1/vision                          Authentication Fails (governor) [HTTP 401]
/v1/this-path-does-not-exist-xyz    Authentication Fails (governor) [HTTP 401]
/v1/banana                          Authentication Fails (governor) [HTTP 401]
/completely/made/up                 Authentication Fails (governor) [HTTP 401]
```

The auth layer answers before anything routes. Only an authenticated
request tells you whether a path is real.
