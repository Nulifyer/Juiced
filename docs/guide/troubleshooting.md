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

1. Find it under **Models → Installed**.
2. Check that its **Overview** tab shows **Weights** as **Present**.
3. Change Disabled to **Hot** or **On demand** if you intend to serve it.
4. Under **Connect → Protocols**, check that the client's protocol is turned on.
5. Refresh the client's model list and check its base URL.
6. Use the exact model ID returned by `/v1/models`.

Disabled models stay visible in the console and are hidden from apps. Some
clients cache their model list.

## A model will not load

1. Open the model under **Models** and read the error on its **Overview** tab.
2. Under **Engines & hardware**, check that the engine build is installed and
   that the device does not show **No access**.
3. On the model's **Settings** tab, check the memory estimate. Lower the
   **Context** if the model does not fit.
4. Open **Diagnostics → Logs** and choose the model as the source.
5. Try a smaller quant if memory is still insufficient.

Installing an engine cannot add a missing host driver or support an unsupported
model architecture. A model that no installed engine can run may need a
different published file. See [runtimes and hardware](runtimes.md).

## A request waits or returns a capacity error

Check active requests, loaded models, and model memory on the Dashboard.
Let busy work finish before you change engines or unload workers.
A Hot model can hold memory that another request needs.

Inspect the operator admission report when needed:

```bash
curl --fail-with-body http://localhost:8080/v1/juiced/admission
```

A full queue answers HTTP `429`. A request that waits too long in the queue
answers `504`. Review the error body and the actual resources before you raise a
limit. The **Model memory** budget is separate from physical memory and from the
engine's slots.

## A conversation does not reuse the cache

1. Open **Cache** and read **Recent turns** for the model.
2. Check the model's mode under **Model overrides**, and any note on its row.
3. Check that the client sends the full message history each turn, or a stable
   `X-Juiced-Session-Id`.

A different API key, a changed launch setting or another engine build starts
the conversation cold. A system prompt that changes every turn also defeats reuse
on hybrid models. See [manage the conversation cache](cache.md).

A slot save from a llama.cpp client returns `409` while the model is in
**Memory and disk** mode. Set the model to **Memory only** or **Off** first.

## A client setting has no effect

Check the response for an `X-Lemonade-Warning` header. Juiced ignores request
fields that the selected protocol or engine cannot use, and names each one there.
The service log records the same warning. For example, `Ignored 'num_thread' option`
means the Ollama client sent a setting that Juiced controls through the model's
loading settings.

## Speech or images fail

Test the selected service directly under **Test services** first. For speech,
check the selected voice and audio format. For images, reduce the requested
size and check sampler, scheduler, and model support.

For Home Assistant, test transcription and synthesis before checking the private
Wyoming TCP connection. A running listener does not prove a successful voice turn.
See [Home Assistant speech](../integrations/home-assistant.md).

## Collect a useful bug report

Record the Juiced version, image identity, model ID, engine build, operation,
and exact error. Include the relevant sanitized log excerpt and reproduction
steps. Remove credentials, prompts, private paths, and client identifiers before
sharing. Report Juiced problems in the
[Juiced issue tracker](https://github.com/Nulifyer/Juiced/issues).
