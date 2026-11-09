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
Set `stream` to `true` for a supported streaming path and use a client that reads
server-sent events. Tool use and vision require model and backend support.

## Choose a service

| Method and path | Requirement |
| --- | --- |
| `GET /v1/models` | Returns enabled consumer models |
| `POST /v1/chat/completions` | Enabled chat model |
| `POST /v1/responses` | Backend implementing the supported Responses subset |
| `POST /v1/embeddings` | Compatible embedding model |
| `POST /v1/audio/transcriptions` | Compatible transcription model, multipart audio upload |
| `POST /v1/audio/speech` | Compatible speech model and voice |
| `GET /v1/audio/voices?model=MODEL_ID` | Voice discovery supported by that speech backend |
| `POST /v1/images/generations` | Compatible image model |

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
Decode or fetch the returned image data in your client. Use a compatible
embedding model for vectors:

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
