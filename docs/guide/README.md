# Run your first model

Use Juiced's browser console to download a chat model and send a test prompt.
You need a [running Juiced server](install/README.md), disk space for the weights,
and enough memory for the model and its context.

## Download a model

1. Open `http://localhost:8080`, or your server's configured address.
2. Select **Library → Discover models**.
3. Choose **Model catalog** and filter the service to **Chat**.
4. Choose a small GGUF model and select **Download**.
5. Open **Library → Downloads** and wait for the download to finish.

Download size describes the weights on disk. Runtime memory also includes
context, engine buffers, and any companion components.

## Choose where it runs

1. Open the downloaded model's details.
2. Select **Runtime**.
3. Choose **llama.cpp** and a compatible hardware variant under **Run on**.
4. Select **Use this profile** for an unloaded model.

Use CPU for a first test if accelerator access is not configured. An unavailable
choice explains which package, hardware, or model requirement is missing.
See [runtimes and hardware](runtimes.md) before selecting an experimental profile.

## Enable and test it

New downloads start **Disabled**.

1. Set the model's **Loading policy** to **Hot** to keep it ready.
2. Wait for its worker to load.
3. Select **Test this variant**, or open **Test services → Chat**.
4. Choose the loaded model and send a short prompt.
5. Check that the model returns a complete answer.

A loaded worker confirms startup. A completed request checks that the selected
model can serve this operation.

The test page is temporary. Leaving the page, changing models, or reloading
clears chat results. Use a connected client for conversations you want to keep.

## Connect an app

Open **Service APIs** and choose your client's protocol. For an OpenAI-compatible
client on the same machine, use `http://localhost:8080/v1` and the exact model ID
shown by Juiced. Follow [connect an application](../integrations/README.md) for
remote addresses, API keys, and compatibility limits.

Choose [On-demand](loading-policy.md) if the model should load only when requested.
