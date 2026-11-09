# Use Juiced's APIs

Send inference requests through the protocol your client supports. All interfaces
use the same managed model library, loading policies, and backend workers.

## Discover models and support

```bash
curl --fail-with-body http://localhost:8080/v1/models
curl --fail-with-body http://localhost:8080/v1/juiced/capabilities
curl --fail-with-body http://localhost:8080/v1/juiced/protocols
```

`/v1/models` lists consumer-visible models. Native capability discovery describes
declared operations and unknowns. Protocol discovery lists implemented subsets.
Neither declaration establishes model quality or hardware qualification.

## Choose an interface

| Interface | Guide | Connection |
| --- | --- | --- |
| OpenAI-compatible | [Chat, embeddings, audio, and images](openai.md) | `/v1` |
| Ollama-compatible | [Chat, generation, and embeddings](ollama.md) | Server root, using `/api/*` |
| Anthropic-compatible | [Messages and token counting](anthropic.md) | Server root, using `/v1/messages` |
| AUTOMATIC1111-compatible | [Text-to-image subset](automatic1111.md) | Server root, using `/sdapi/v1/*` |
| MCP | [Model-service tools](mcp.md) | `/mcp` |
| llama.cpp-specific | [Reranking and native operations](llamacpp.md) | `/v1` |
| Juiced management | [Discovery, profiles, and lifecycle](juiced.md) | `/v1/juiced/*` |

Use the exact enabled model ID from discovery. Add the configured
[API credential](../guide/access.md) to the examples when authentication is enabled.

## Handle limits

Model and engine support determine which operations, request fields, streaming
modes, and context sizes work. Explicit supported request parameters override
model defaults. Invalid requests and admission limits still apply.

Tools returned by a model execute in the client. Provider-hosted tools and stored
consumer conversations are outside the model service. Generic user-authored job
graphs and legacy Omni agent loops are unavailable in Juiced.

Qualified persistent cache reuse is limited to supported Chat Completions profiles.
Use [Memory mode](../guide/cache.md) for incompatible protocol paths.
