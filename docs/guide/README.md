# Run your first model

Use Juiced's browser console to download a chat model and send a test prompt.
You need a [running Juiced server](install/README.md), disk space for the weights,
and enough memory for the model and its context.

The console follows your system's light or dark setting. To pick one for this
browser, open the service status menu, select **Connection settings** and choose
**Auto**, **Dark** or **Light** under **Theme**.

## Download a model

1. Open `http://localhost:8080`, or your server's configured address.
2. Select **Models**, then the **Discover** tab.
3. Pick one of the models suggested for this machine and select **Download**.
   Juiced fetches the quant shown and adds the model as **On demand**, which
   loads it on its first request.
4. For another model, search, for example `qwen3`, or set a filter such as the
   **Chat** type. Select a model's name to open the detail panel. It lists every
   quant with its size and whether it fits this machine, and lets you choose the
   **Loading policy** before **Download**.
5. The **Downloads** tab shows progress, errors, **Pause** and **Retry**.

Download size describes the weights on disk. Runtime memory also includes
context, engine buffers, and any companion files.

## Add the model

When the download finishes, Juiced adds the model with the policy you chose. If
the model needs an engine that is not installed, Juiced installs the build it
picked for this machine first. Select **Open model** when the panel says
**Added** to see it in **Models → Installed**.

To run on the CPU instead of a GPU, open the model's **Settings** tab and select
**CPU only** under **Run on**. See [runtimes and hardware](runtimes.md) to install
another engine build.

## Test it

1. Open **Test services**. The **Try a model** tab opens.
2. Choose the model. A model that is not loaded shows **Loads on first use**.
3. Send a short prompt and check that the model returns a complete answer.

The model's **Overview** tab also has **Load and check**, which loads the model
and sends a few short real requests.

The test page is temporary. Leaving the page, changing models, or reloading
clears chat results. Use a connected client for conversations you want to keep.

## Connect an app

Open **Connect** and choose your client's protocol under **Protocols**. Copy the
address shown for it. For an OpenAI-compatible client on the same machine, the
address is `http://localhost:8080/v1`. Under **How to connect**, pick the model
and select **Copy ID** to keep its spelling intact. Follow
[connect an application](../integrations/README.md) for remote addresses, API
keys, and compatibility limits.

Choose [Hot](loading-policy.md) if the model should stay loaded and ready.
