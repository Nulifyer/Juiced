# Call model services through MCP

Connect an MCP client to `http://localhost:8080/mcp` to call individual model
operations. Juiced uses Streamable HTTP with the tools capability and protocol
version `2025-06-18`.

## Discover tools

```bash
curl --fail-with-body http://localhost:8080/mcp \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}'
```

Add the inference key when authentication is configured.
Use the returned names and argument schemas to configure calls in your client.
Some tool names retain the `lemonade_` prefix for compatibility.

The gateway accepts JSON-RPC requests through `POST /mcp`. The separate
server-initiated SSE channel through `GET /mcp` is unavailable.

## Enable models before calling

Download and enable the intended model in Juiced. The model-service gateway
filters selection through the same loading policy and does not auto-enable or
download a default in response to inference. Any inherited tool-schema text
about download opt-in does not override Juiced's policy enforcement.

Use Memory cache mode when a persistent deployment rejects the unleased MCP
request path. The client owns orchestration and any tool execution beyond these
individual model operations. Generic caller-authored job graphs and Omni agent
loops are unavailable.

For exact schemas, query `tools/list` against the running release. The native
[generated MCP reference](https://github.com/Nulifyer/Juiced/blob/main/juiced/MCP-TOOLS.md) records the build's tool
metadata for contributors.
