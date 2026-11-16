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
3. Search for a model, for example `qwen3`, and select the **Chat** task.
4. Select a quant chip, such as **Q4_K_M**, on a small GGUF model. The detail
   panel lists every quant with its size and whether it fits this machine.
5. Select **Download**. The quant shows **✓ downloaded** when the files are on disk.
   The **Downloads** tab shows progress, errors, **Pause** and **Retry**.

Download size describes the weights on disk. Runtime memory also includes
context, engine buffers, and any companion files.

## Add the model

1. In the same detail panel, choose a policy under **Make available as**.
   **On demand** is selected by default and loads the model on its first request.
2. Select **Add model**. If the model needs an engine that is not installed,
   Juiced names the engine and its download size. Select **Install engine and add**.
3. Select **Open model** to see it in **Models → Installed**.

Juiced picks the engine build for this machine. To run on the CPU instead of a
GPU, open the model's **Settings** tab and select **CPU only** under **Run on**.
See [runtimes and hardware](runtimes.md) to install another engine build.

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
