# Use the Anthropic-compatible API

Configure the client's server address as `http://localhost:8080` and select an
enabled chat model. The message endpoint is `POST /v1/messages`.

## Send a message

```bash
curl --fail-with-body http://localhost:8080/v1/messages \
  -H 'Content-Type: application/json' \
  -H 'anthropic-version: 2023-06-01' \
  -d '{"model":"your-enabled-chat-model-id","max_tokens":64,"messages":[{"role":"user","content":"Say hello."}]}'
```

Replace the example model ID with an enabled model from `/v1/models`.
Use the configured inference credential through `x-api-key` or Bearer
authentication. If both are present, Bearer takes precedence.

## Check supported behavior

The interface translates the supported Messages, tool-use, and streaming subset
through the selected worker. Tools execute in the client. Provider-hosted tools
are not implemented.

`POST /v1/messages/count_tokens` requires backend-native message token counting.
The bundled llama.cpp b12032 contract supplies that capability. Do not assume
another engine implements it.

Use **Memory** cache mode for this protocol when a qualified persistent
Chat Completions deployment rejects its request path. Model behavior and tool
quality require a test in the actual client.
