# Use the Ollama-compatible API

Configure the client's Ollama host as `http://localhost:8080`.
Keep `/v1` out of the base address. The protocol uses `/api/*` paths.

## Discover models

```bash
curl --fail-with-body http://localhost:8080/api/tags
curl --fail-with-body http://localhost:8080/api/ps
```

Tags list enabled chat, completion and embedding models as `ID:latest`. The
process list shows the loaded ones. Speech, transcription and image models are
not listed. Use the exact model name returned by discovery.

Each entry has `modified_at`, `size` (bytes on disk), `digest` and `details`
with `format`, `family`, `families`, `parameter_size` and `quantization_level`.
`family` is the GGUF architecture, such as `qwen3`. `parameter_size` comes from
the GGUF size label or the model name. A value Juiced cannot read is an empty
string. `digest` is the file's verified SHA-256 when Juiced downloaded it, else a
hash of the file's path, size and modification time.

`/api/ps` entries add `size` (memory the model holds), `size_vram` (the same on
a GPU engine, else 0), `expires_at` (when an idle On demand model unloads, or
`2099-01-01T00:00:00Z` for Hot models) and `context_length`.

`POST /api/show` returns `capabilities` (`completion`, `embedding`, `tools`,
`vision`, `thinking`), `details`, `model_info` with `general.architecture` and
`<architecture>.context_length`, and `parameters` as `num_ctx N`. The context is
what one request can use on the running worker, else the model's trained
context. `tools` and `thinking` follow the model's chat template, the same
values `/v1/models` reports.

## Send a request

```bash
curl --fail-with-body http://localhost:8080/api/chat \
  -H 'Content-Type: application/json' \
  -d '{"model":"your-enabled-chat-model-id","messages":[{"role":"user","content":"Say hello."}],"stream":false}'
```

Replace the example model ID with one from `/api/tags`. Add your configured
inference credential when needed.

## Supported subset

| Endpoint | Operation |
| --- | --- |
| `GET /api/tags` | Enabled model discovery |
| `POST /api/show` | Read named-model metadata |
| `GET /api/ps` | Loaded model information |
| `POST /api/chat` | Chat and supported tool calls |
| `POST /api/generate` | Prompt completion |
| `POST /api/embed` | Embeddings |
| `POST /api/embeddings` | Legacy single-prompt embeddings |

`POST /api/show` is a metadata read. Use Juiced's Library and model management
controls for acquisition and removal. These requests return HTTP 501:

| Request | Error |
| --- | --- |
| `POST /api/pull` | `Use Juiced model management to download weights` |
| `DELETE /api/delete` | `Use Juiced model management to remove weights` |
| `POST /api/create`, `/api/copy`, `/api/push`, `/api/blobs/DIGEST` | `not supported by Lemonade` |

## Options and ignored fields

Juiced forwards `temperature`, `top_p`, `top_k`, `min_p`, `typical_p`, `seed`,
`stop`, `presence_penalty`, `frequency_penalty`, `repeat_penalty`,
`repeat_last_n` and the `mirostat` settings to the engine. `num_predict` sets the
output limit; `-1` and `-2` mean no limit. The options can also appear at the top
level, where they override `options`. `num_ctx` is accepted, but the context size
comes from the model's loading settings.

`think: false` turns reasoning off on models whose chat template supports it.

Other options and fields are ignored, not rejected. Examples are `num_thread`,
`num_gpu`, and `system`, `context` or `raw: false` on `/api/generate`. Generation
sends the prompt to the model without a chat template. Each ignored part is
named in the `X-Lemonade-Warning` response header.

Select **Memory** cache mode for models used through this interface when
persistent mode rejects the request. Tool-call execution belongs to the client.
