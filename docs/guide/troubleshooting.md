# Diagnose a problem

Start with the failing operation, its model, and the error shown by Juiced.
Open **Diagnostics** for performance, request metadata, and logs.

## The console will not open

1. Check the container state with `docker ps -a --filter name=juiced`.
2. Read startup output with `docker logs --tail 100 juiced`.
3. Check the manager with `curl --fail http://localhost:8080/v1/juiced/state`.
4. Check the published port and your proxy or firewall rules.

Add your operator credential when HTTP authentication is configured.
A loopback-only Docker binding cannot be reached directly from another machine.
If startup reports permission errors, verify the data mount and UID/GID `10001`.

## A model is missing from a client

1. Find it under **Library → Downloaded**.
2. Check that it is available and has all required components.
3. Change Disabled to **Hot** or **On-demand** if you intend to serve it.
4. Refresh the client's model list and check its base URL.
5. Use the exact model ID returned by `/v1/models`.

Disabled models remain visible to the operator and are excluded from consumer
model discovery. Some clients cache their model list.

## A model will not load

1. Open its **Runtime** pane and inspect the compatibility explanation.
2. Check package status under **Backends & hardware**.
3. Check device access and available memory.
4. Read the model operation error and backend logs.
5. Try a supported smaller context or model if memory is insufficient.

Installing a package cannot add a missing host driver or unsupported model
architecture. An unavailable runtime may need a different publisher artifact.
Use [runtime selection](runtimes.md) before attempting conversion.

## A request waits or returns a capacity error

Check active requests, resident models, and accelerator ownership in Diagnostics.
Let busy work finish before changing profiles or unloading workers.
A Hot model can retain resources needed by another request.

Inspect the operator admission report when needed:

```bash
curl --fail-with-body http://localhost:8080/v1/juiced/admission
```

Capacity rejection uses HTTP `429` in the native admission path. Review the error
body and actual resources before raising a limit. A configured model-count limit
is separate from physical memory or backend slot availability.

## Caching fails with a protocol error

Persistent reuse is restricted to qualified Chat Completions profiles.
Select **Memory** under **Cache → Model overrides** for a model used through
Ollama, Anthropic, Responses, or MCP when that path is rejected.
For supported persistent reuse, check the stable session ID and full current
message history. Key or profile changes can require a cold rebuild.

## Speech or images fail

Test the selected service directly under **Test services** first. For speech,
check the selected voice and audio format. For images, reduce the requested
size and check sampler, scheduler, and model support.

For Home Assistant, test transcription and synthesis before checking the private
Wyoming TCP connection. A running listener does not prove a successful voice turn.
See [Home Assistant speech](../integrations/home-assistant.md).

## Collect a useful bug report

Record the Juiced version, image identity, model ID, selected profile, operation,
and exact error. Include the relevant sanitized log excerpt and reproduction
steps. Remove credentials, prompts, private paths, and client identifiers before
sharing. Report Juiced problems in the
[Juiced issue tracker](https://github.com/Nulifyer/Juiced/issues).
