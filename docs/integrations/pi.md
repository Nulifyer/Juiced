# Connect a coding client or Pi

Use Juiced as the inference provider while your coding client owns tools,
workspaces, approvals, and saved sessions.

## Configure an OpenAI-compatible provider

1. Download and enable a chat model with the capabilities your client requires.
2. Complete a short chat test in Juiced.
3. Open the client's provider configuration.
4. Choose its OpenAI-compatible or custom provider type.
5. Set the base URL to your reachable Juiced address with `/v1`.
6. Supply the configured inference key and exact model ID.
7. Run a small task and inspect the client's result and Juiced's worker activity.

Configuration keys and provider names depend on the client and version.
Use its own provider documentation for the exact configuration file format.

## Use model discovery

`GET /v1/models` lists enabled models. `GET /v1/juiced/catalog` gives each model's
per-request context, output limit, image input, thinking and tool-call support in one
request, with the evidence behind each value. A Juiced-aware integration reads
`context.per_request_tokens` instead of dividing a launch total by slots, and
switches thinking with `chat_template_kwargs.enable_thinking` when
`thinking.request_control` names it. Send `If-None-Match` with the last `ETag` to
refresh cheaply. It should preserve public model IDs and treat `null` as unknown.
See [Juiced management](../api/juiced.md#discover-models-for-a-client).

A bundled model label does not establish tool-use quality. Test a small file
inspection or tool call in the actual client before assigning a long task.
Juiced returns tool calls. The client decides whether and how to execute them.

## Check cache and context

Use Memory mode if the client's protocol path cannot use qualified persistent
snapshots. For supported Chat Completions session reuse, send full current
history and a stable `X-Juiced-Session-Id` for each conversation.

Configure the client's context budget from `context.per_request_tokens`. The launch
context is a total that llama.cpp splits across slots. See [model settings](../guide/settings.md)
and [cache](../guide/cache.md).

This repository does not bundle a universal Pi/T3 provider installer. Existing
client-specific adapters have their own installation and discovery behavior.
