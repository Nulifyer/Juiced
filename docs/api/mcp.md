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

## Tools

| Tool | Result |
| --- | --- |
| `lemonade_list_models` | Enabled downloaded models, with `loaded`, `available` and `recommended_chat_model` |
| `lemonade_chat` | One chat completion. Thinking is off unless `chat_template_kwargs.enable_thinking` is `true` |
| `lemonade_complete` | One prompt completion |
| `lemonade_embed` | Embeddings |
| `lemonade_transcribe_audio` | The transcript from `audio_base64`, then the full JSON response |
| `lemonade_generate_image` | One PNG `image` content block per image, up to 4 |
| `lemonade_docs` | This server's API reference pages, the same as `GET /v1/docs` |

Chat, transcription and image tools can omit `model`. Juiced then uses a loaded
model for that operation, or else a downloaded one. If neither exists, the tool
returns `isError: true` and asks for a model.

Juiced runs in its own container, so it reads and writes no files for MCP
callers. Send audio as `audio_base64`; one request can carry about 1.5 MB of
audio. `audio_path`, `output_path` and `output_dir` return a tool error.

## Enable models before calling

Download and enable the intended model in Juiced. The gateway uses the same
loading policy and never downloads a model in response to inference.
`allow_download` is accepted and ignored.

Use Memory cache mode when a persistent deployment rejects the unleased MCP
request path. The client owns orchestration and any tool execution beyond these
individual model operations. Generic caller-authored job graphs and Omni agent
loops are unavailable.

For exact schemas, query `tools/list` against the running release.
