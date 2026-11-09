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

`POST /api/show` is a metadata read. Model creation, copy, push, and blob upload
are not implemented. Use Juiced's Library and model management controls for
acquisition and removal.

Select **Memory** cache mode for models used through this interface when
persistent mode rejects the request. Tool-call execution belongs to the client.
