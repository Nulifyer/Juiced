# Use the Ollama-compatible API

Configure the client's Ollama host as `http://localhost:8080`.
Keep `/v1` out of the base address. The protocol uses `/api/*` paths.

## Discover models

```bash
curl --fail-with-body http://localhost:8080/api/tags
curl --fail-with-body http://localhost:8080/api/ps
```

Tags list enabled models. The process list reports loaded workers.
Use the exact model name returned by discovery.

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
