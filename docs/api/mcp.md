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

## Manage Juiced from a model

Callers with an operator key, or any caller in trusted-LAN mode, also get
`juiced_` management tools. Inference keys see only the model-service tools
above, and a management call made with one fails with JSON-RPC error `-32001`.
Each tool runs the matching native route with the caller's own key, so the
route's validation and revision checks apply unchanged.

| Tool | Changes state | Native route |
|---|:---:|---|
| `juiced_status` | no | `GET /v1/juiced/state`, `GET /v1/system-info`, `GET /v1/juiced/catalog` |
| `juiced_list_models` | no | `GET /v1/juiced/catalog` |
| `juiced_model_settings` | no | `GET /v1/models/{model}/options` and `/fit` |
| `juiced_list_engines` | no | `GET /v1/juiced/engines` |
| `juiced_search_models` | no | `GET /v1/juiced/library/search` |
| `juiced_list_local_files` | no | `GET /v1/juiced/library/local` |
| `juiced_list_downloads` | no | `GET /v1/juiced/library/pulls` |
| `juiced_set_policy` | yes | `POST /v1/juiced/policy` |
| `juiced_load_model` | yes | `POST /v1/load` |
| `juiced_unload_model` | yes | `POST /v1/unload` |
| `juiced_set_model_settings` | yes | `POST /v1/models/{model}/options` |
| `juiced_download_model` | yes | `POST /v1/juiced/library/pull` |
| `juiced_add_model` | yes | `POST /v1/juiced/models/add` |
| `juiced_install_engine` | yes | `POST /v1/juiced/engines/install` |

`tools/list` marks each tool with `readOnlyHint`, plus `destructiveHint` and
`idempotentHint` on the ones that change state. Results arrive as one JSON
`text` block and the same value in `structuredContent`. A route failure, such
as `revision_conflict` from a stale `expected_revision`, comes back as
`isError: true` with the route's error code and HTTP status. Settings fields you
omit keep their saved value, and `null` resets one to automatic. Downloads and
engine installs read the current generation when you leave
`expected_generation` out, as the console does.

Browsers can call `/mcp` only from the origin that served the page, which is how
the console's Assistant reaches it. Other browser origins get `403`.

For exact schemas, query `tools/list` against the running release. The native
[generated MCP reference](https://github.com/Nulifyer/Juiced/blob/main/juiced/MCP-TOOLS.md) records the build's tool
metadata for contributors.
