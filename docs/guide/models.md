# Download or import models

Get weights through **Models → Discover** or from a local file. Then add the model
with the loading policy you want.

## Find and download a model

1. Open **Models → Discover**.
2. Search by name, for example `qwen3`, `llama` or `whisper`. Narrow the results
   with a task chip such as **Chat** or **Speech to text**, or pick an engine in
   **Any engine**.
3. Select a quant chip on a result. **Recommended** marks a model from Juiced's
   catalog, ★ marks the suggested quant, and **✓ On this machine** means a quant
   is already downloaded. **+N more** opens the full quant list.
4. In the detail panel, compare the quants. Each one shows its size and a fit
   estimate under **This machine**, such as **Fits in GPU memory** or
   **Runs on the CPU**.
5. Select **Download**. Progress shows in the panel and under **Downloads**.

The fit estimate compares the file size with GPU and system memory. It does not
include context or engine buffers. **Size unknown** means the publisher did not
report a size.

A quant is a published weight format such as `Q4_K_M` or `Q6_K`. Smaller quants
use less storage and memory, with some loss of precision. Juiced downloads
published quants. It does not convert or quantize weights.

If Hugging Face cannot be reached, Discover shows recommended and downloaded
models only.

## Add the model

When the quant shows **✓ downloaded**, the detail panel offers **Add model**.

1. Choose a policy under **Make available as**: **Disabled**, **On demand** or
   **Hot**. **On demand** is the default.
2. Select **Add model**.
3. If the model needs an engine that is not installed, Juiced names it. Select
   **Install engine and add**.
4. Select **Open model** to see the new model under **Models → Installed**.

Juiced picks the engine and hardware build for this machine. See
[choose loading policies](loading-policy.md) and [runtimes and hardware](runtimes.md).

## Download an original checkpoint

Some engines run a model's original folder instead of a GGUF file:

| Checkpoint | Engine | What Juiced downloads |
| --- | --- | --- |
| Transformers (`config.json` with `.safetensors` weights) | vLLM | Weights, shard index, config, generation config, tokenizer and chat template files |
| Exported ONNX classifier (`model.onnx`, `tokenizer.json`, `config.json`) | ONNX Runtime | The graph and its external data, tokenizer, vocabulary and config |
| Split diffusion files (`diffusion_models/`, `vae/`, `text_encoders/`) | TheNoise | One diffusion model with its VAE and text encoder |

1. Open **Models → Discover** and search, for example `Qwen3-0.6B`. Pick an engine
   in **Any engine** to see only what it runs. The **Classify** task lists text
   classifiers.
2. A checkpoint shows as one chip, such as **Checkpoint · vLLM**. Select it to see
   every file, the total size, the license and the engine. READMEs, images and
   other formats are skipped.
3. Select **Download**. The files download at the repository's current commit and
   appear as one row under **Downloads**.
4. When the checkpoint shows **✓ downloaded**, select **Add model**. Juiced names
   the model after the repository and registers it on the engine shown. If that
   engine is not installed, select **Install engine and add**.

vLLM and TheNoise need a supported GPU. A checkpoint can still be downloaded on a
host without one, but adding it reports `no_usable_backend`.

Gated repositories need a Hugging Face token. Set `HF_TOKEN` on the server and
accept the repository's license on Hugging Face. Juiced sends the token only to
Hugging Face, never to the download mirrors it redirects to.

Files that two models share are stored once. Removing a model deletes only the
files no other model uses. See [manage storage](storage.md).

## Follow downloads

Open **Models → Downloads**. The tab shows a count while transfers are active.
Model downloads and engine installs both appear here, with file progress and
errors.

- **Pause** stops a running transfer. **Resume** continues a paused or
  interrupted one.
- **Retry** starts a failed or interrupted transfer again.
- **Cancel** stops a transfer for good. **Dismiss** removes a finished or
  cancelled row.
- A finished download offers **Add model**, which opens its file details, or
  **Open model** once a model uses the files.

Downloads continue after a restart from the bytes already on disk. Paused ones
stay paused. Downloading never registers or loads a model by itself.

To download one file from another site, select **Add file → Download from a URL…**.
Enter the **Download URL** and the **Publisher SHA256**, then select
**Start download**. Juiced keeps the file only when its size and checksum match.

## Import a local file

1. Open **Models → Discover** and select **Add file**.
2. Select **Upload from this computer…** to send files to Juiced, or
   **Pick from the server…** to use files the server can already read under
   `import_roots`. For Docker, that is a container path such as `/library`, not
   the path on your laptop.
3. For a file on the server, choose under **Keep the file**:
   **Where it is** uses the file in place, and **Copy to library** copies it into
   Juiced's storage and checks its digest.
4. Check the **Model name** and, for a vision model, the **Vision projector**.
5. Choose a policy under **Make available as** and select **Add model**.

A file used in place must stay at the same path. Moving or deleting it breaks
the model.

A split GGUF keeps its parts as files named `Model-00001-of-00003.gguf`,
`Model-00002-of-00003.gguf` and so on. Keep those names, because llama.cpp finds
the other parts from the first one's name.

- To upload a split model, select every part in the file picker at once. Juiced
  checks that no part is missing before it uploads anything, then uploads the
  parts one at a time. If an upload fails partway, the parts that finished stay
  in the library. Pick the same files again to finish.
- On the server, the folder shows a split model once, as its first part with the
  part count and total size. Split models can only be used in place, so pick
  **Where it is**. If a part is missing, Juiced names it and adds nothing.

A vision model needs its projector, usually a file named `mmproj-…gguf`.

- When uploading, select the projector together with the model file or parts.
- When picking from the server, Juiced offers the projectors in the model's
  folder under **Vision projector** and selects the first one. Choose
  **None (text only)** to add the model without image input.

Uploading or picking only a projector is refused, because it cannot run alone.
The memory estimate counts every part and the projector.

## Load and check a model

1. Open the model under **Models → Installed**.
2. On the **Settings** tab, review the memory estimate and **Launch settings**.
   **Save** is unavailable while a changed setting would not fit the memory
   budget.
3. On the **Overview** tab, choose a [loading policy](loading-policy.md) and
   select **Load**. While it loads, the state shows the time so far, and the
   time left when the model loaded on the same engine before.
4. Under **Check**, select **Run check**, or **Load and check** for a model that
   is not loaded. Then [complete a service test](testing.md).

Keep the exact model ID when you configure clients. Each quant is a separate
model with its own settings and loading policy.

## Work with several models

Tick models in **Models → Installed** to **Load**, **Unload** or **Set policy…**
for all of them at once. Select the **Model**, **State** or **Memory** column
title to sort. The chips above the list filter by service. **Enabled** and
**Disabled** split the list; Juiced shows Enabled models when there are any, and
a search also lists Disabled models that match. **Not added** lists downloaded
files that no model uses yet.

With a model open, press Escape to close it. The arrow keys move between its
tabs.

**Check for updates** compares each downloaded Hugging Face model with the
current upstream file and marks changed ones **update available**. It downloads
nothing. Scripts use `POST /v1/models/check-updates`. `/v1/models` then reports
`update_available` per model until the service restarts.

When Juiced knows a model's repository, the model's details link to its model
card and README on Hugging Face.

## Remove a model

1. Open the model and set its loading policy to **Disabled**.
2. Open the **Manage** tab.
3. Select **Remove model**, then **Remove model** again to confirm.

Removing a model keeps its weight files. Files no other model uses then
show under **Not added**. Open them and select the quant's **Delete** button to
free the space, or use [Reclaim space](storage.md#reclaim-space). Files in a read-only folder are
never deleted.

See [choose a published variant](variants.md) to add another quant while keeping
the original model.
