# Use the OpenAI-compatible API

Configure an OpenAI-compatible client with `http://localhost:8080/v1`.
Choose an enabled model that implements the operation you need.

## Send a chat request

Use the exact chat model ID returned by `GET /v1/models` in place of the example ID:

```bash
curl --fail-with-body http://localhost:8080/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"your-enabled-chat-model-id","messages":[{"role":"user","content":"Reply with a short greeting."}],"max_tokens":64,"stream":false}'
```

For authenticated deployments, add `Authorization: Bearer` with your inference key.

## Turn thinking on or off per request

Send `chat_template_kwargs.enable_thinking` with `true` or `false`. Juiced passes it
to llama-server, which renders it into the model's chat template. Check
`thinking.request_control` in the model's catalog entry first; models whose template
has no switch ignore it.

```json
{"model":"your-thinking-model","messages":[{"role":"user","content":"Hi"}],"chat_template_kwargs":{"enable_thinking":false}}
```

Native clients' forms also work on `/v1/chat/completions`: a top-level
`enable_thinking` boolean, `thinking: true|false` and
`thinking: {"type":"enabled"|"disabled"}`. Juiced moves them into
`chat_template_kwargs.enable_thinking` and drops the top-level fields; a value already
in `chat_template_kwargs` wins. Anthropic `thinking` and Ollama `think` map the same
way. Juiced does not add `reasoning_effort` and does not prepend `/no_think`; a
`reasoning_effort` you send reaches llama-server unchanged. Under a protected request
profile these fields are refused.
Set `stream` to `true` for a supported streaming path and use a client that reads
server-sent events. Tool use and vision require model and backend support.

## Choose a service

| Method and path | Requirement |
| --- | --- |
| `GET /v1/models` | Returns enabled consumer models with native fields |
| `POST /v1/chat/completions` | Enabled chat model |
| `POST /v1/responses` | Backend implementing the supported Responses subset |
| `POST /v1/embeddings` | Compatible embedding model |
| `POST /v1/audio/transcriptions` | Compatible transcription model, multipart audio upload |
| `POST /v1/audio/speech` | Compatible speech model and voice |
| `GET /v1/audio/voices?model=MODEL_ID` | Voice discovery supported by that speech backend |
| `POST /v1/images/generations` | Compatible image model |
| `POST /v1/images/edits` | Compatible image model, multipart image upload |
| `POST /v1/images/variations` | Compatible image model, multipart image upload |
| `POST /v1/images/upscale` | Validated only; Juiced has no upscaling models |

Each `GET /v1/models` entry has the OpenAI fields plus `recipe`, `downloaded`,
`labels`, `context`, `context_length` (tokens one request can use), `vision`,
`thinking`, `tool_calls` and, when the launch sets `--predict`, `max_output_tokens`
and `max_completion_tokens`. [Juiced management](juiced.md#discover-models-for-a-client)
defines them. Aliases appear as their own entries with `aliased_to`.
`?show_all=true` also lists disabled and missing models, `?service=chat|transcribe|speak|images|embeddings`
filters by model type, and an `anthropic-version` header returns the Anthropic model
list shape with chat models only. Responses carry a weak `ETag`; send `If-None-Match`
to get `304` when the list is unchanged.

Responses support does not include provider-hosted tools or stored conversation
management. Token counting, structured output, vision, and tool behavior depend
on the selected engine/profile.

## Transcribe an audio file

Set `JUICED_TRANSCRIPTION_MODEL` to an enabled transcription model ID and use a
local audio file:

```bash
curl --fail-with-body http://localhost:8080/v1/audio/transcriptions \
  -F "model=$JUICED_TRANSCRIPTION_MODEL" \
  -F 'file=@speech.wav' \
  -F 'language=en'
```

The returned transcription depends on the selected backend and audio format.
Use a short WAV file for a first check.

## Generate speech

Choose an enabled speech model and a voice it supports. Replace both example
values before sending the request:

```bash
curl --fail-with-body http://localhost:8080/v1/audio/speech \
  -H 'Content-Type: application/json' \
  -d '{"model":"your-enabled-speech-model-id","input":"Hello from Juiced.","voice":"your-supported-voice","response_format":"wav"}' \
  --output speech.wav
```

Check the response status before treating the output file as audio.
The voice list is available through `GET /v1/audio/voices?model=MODEL_ID` when
supported by the backend. URL-encode the model ID in that query.

## Generate images or embeddings

Use an enabled image model for an image request:

```bash
curl --fail-with-body http://localhost:8080/v1/images/generations \
  -H 'Content-Type: application/json' \
  -d '{"model":"your-enabled-image-model-id","prompt":"A wooden cabin in a snowy forest","size":"512x512","n":1}'
```

Image output format depends on the backend and supported request settings.
Decode or fetch the returned image data in your client.

### Edit images

Send `multipart/form-data`, as the OpenAI SDK's `images.edit` does:

```bash
curl --fail-with-body http://localhost:8080/v1/images/edits \
  -F model=your-enabled-image-model-id \
  -F "prompt=Add a red barn in the background" \
  -F image=@source.png
```

- `image` (one file) or repeated `image[]` (up to 8 references, in order): PNG, JPEG or WebP.
- Optional `mask`, `n` (1 to 10), `size` (`WIDTHxHEIGHT`, multiples of 8 up to 2048),
  `output_format` (`png` or `jpeg`), `steps`, `cfg_scale`, `seed`, `negative_prompt`.
- Without `size`, the output keeps the first image's size, scaled down to at most
  2048 pixels per side.
- `user`, `quality`, `background`, `input_fidelity` and `output_compression` are
  accepted and ignored. The whole form can be up to 8 MiB.

`/v1/images/variations` takes the same form with exactly one `image`, no mask and
no prompt. Editing quality depends on the model; editing-capable models such as
Flux Klein work best.

`/v1/images/upscale` checks its JSON body (`image` as base64, `model`) like Juiced
0.9 did, but Juiced registers no ESRGAN upscaling models. An unknown model returns
`404`, and any registered model returns `400`.

Use a compatible embedding model for vectors:

```bash
curl --fail-with-body http://localhost:8080/v1/embeddings \
  -H 'Content-Type: application/json' \
  -d '{"model":"your-enabled-embedding-model-id","input":"A short sentence to embed."}'
```

## Reuse a prompt snapshot

For a qualified persistent Chat Completions profile, send the full current history
and a stable `X-Juiced-Session-Id` header. Do not reuse a session ID across
unrelated conversations. Other protocols have different cache restrictions.
See [cache and identity](../guide/cache.md).

Native OpenAI-style and management routes also register `/v0/`, `/api/v0/`, and
`/api/v1/` aliases. Use `/v1/` consistently for a new client.
