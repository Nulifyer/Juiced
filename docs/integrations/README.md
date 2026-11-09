# Connect an application

Point your client at Juiced and choose an enabled model.
First [complete a local test](../guide/testing.md) for the service you need.

## Choose the protocol

Open **Service APIs** in Juiced. Copy the connection address for your client's
protocol and review the listed limits.

| Client type | Address on the Juiced host | Model selection |
| --- | --- | --- |
| OpenAI-compatible | `http://localhost:8080/v1` | Exact ID from `GET /v1/models` |
| Ollama-compatible | `http://localhost:8080` | Exact name from `GET /api/tags` |
| Anthropic-compatible | `http://localhost:8080` | Enabled chat model ID |
| AUTOMATIC1111-compatible | `http://localhost:8080` | Enabled sd-cpp image model |
| MCP | `http://localhost:8080/mcp` | Model argument of the discovered tool |
| Wyoming speech | Private TCP host, port `10300` | Enabled speech models advertised by the listener |

For remote clients, replace `localhost` with the server's reachable hostname.
For another container, use a shared private network address or a reachable host
address. Each container has its own localhost.

## Configure the client

1. Choose its custom, self-hosted, or compatible provider option.
2. Set the base URL for that protocol.
3. Supply the inference API key if Juiced requires one.
4. Refresh models, or enter the exact enabled model ID.
5. Send a short request and confirm the selected worker served it.

If the client requires a non-empty key in an anonymous local deployment, use a
dummy value. That value adds no authentication. Read
[access and API keys](../guide/access.md) before opening remote access.

## Check compatibility

Compatibility applies to documented subsets. A client that requires provider-hosted
tools, stored conversations, or another unsupported feature may need different
settings. Tool-call output is returned to the client for execution.
Juiced's model service does not execute the client's tools.

Use Memory cache mode for request paths that cannot use persistent snapshots.
Clients using qualified Chat Completions reuse can supply a stable
[session header](../guide/cache.md).

See [API examples](../api/README.md), [coding clients and Pi](pi.md), or
[Home Assistant speech](home-assistant.md).
