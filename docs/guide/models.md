# Download or import models

Get weights through **Models → Discover** or from a local file. Then add the model
with the loading policy you want.

## Find and download a model

1. Open **Models → Discover**. It opens on up to six models suggested for this
   machine. **Download** on a row fetches the quant shown and adds the model as
   **On demand**. See [suggested models](#suggested-models).
2. For anything else, search by name, for example `qwen3`, `llama` or `whisper`,
   or set a filter beside the search box. Search text or any filter replaces the
   suggestions with results from Juiced's catalog, Hugging Face and this
   machine. Filters combine, and **Clear filters** resets them. With no search
   text and no filter, the suggestions return.

   | Filter | Choices |
   |---|---|
   | **Any type** | What the model does, such as **Chat** or **Speech to text** |
   | **Any engine** | The engine that runs it, such as llama.cpp, vLLM or FastFlowLM |
   | **Any hardware** | **Fits GPU**, **Fits memory** (GPU or system memory) or **NPU** |
   | **Any size** | The size of a quant, from **Under 2 GiB** to **Over 24 GiB** |
   | **Best match** | The order of search results: **Most downloads** or **Smallest first** |

   With a hardware or size filter, each result offers a quant that matches.
   Results load 12 repositories at a time. When there are more, the count reads
   **First 12 models** and **Show more** loads the next page. Hardware and size
   are checked against the loaded pages only, so the count then reads, for
   example, **3 of the first 24 models match**. **Search more** appears when no
   loaded model matches yet.
3. Each result is one row: the model, one quant with its size and a fit dot, and
   one action. The quant is the one on disk if there is one, then the
   recommended one, then a common quant that fits this machine. **Recommended**
   marks a model from Juiced's catalog. **Download** fetches that quant and adds
   the model as **On demand**. A row marked **✓ On disk** offers **Open model**
   or **View** instead. **View** also replaces **Download** for a quant that is
   too big for this machine, gated or not downloadable. Select the model's name
   or **N quants** to open the detail panel with every quant.
4. In the detail panel, compare the quants. Each row shows a fit dot, the quant,
   its state on this machine and its size. A quant on disk is a model, so its
   state is that model's loading policy. An unfinished split download shows the
   parts on disk. **This machine** spells out the fit, such as
   **Fits in GPU memory** or **Runs on the CPU**. A quant on disk offers
   **Open model** and **Remove** instead of **Download**.
5. Choose a **Loading policy**: **Disabled**, **On demand** or **Hot**.
   **On demand** is the default.
6. Select **Download**. Progress shows in the panel and under **Downloads**.
   When the download finishes, the model is under **Installed** with that
   policy. For a
   split download that stopped part way, the button reads **Finish download**
   and fetches only the missing parts.

The fit estimate compares the file size with GPU and system memory. It does not
include context or engine buffers. **Size unknown** means the publisher did not
report a size.

A quant is a published weight format such as `Q4_K_M` or `Q6_K`. Smaller quants
use less storage and memory, with some loss of precision. Juiced downloads
published quants. It does not convert or quantize weights.

If Hugging Face cannot be reached, Discover shows recommended and downloaded
models only, and no suggestions, because model sizes are unknown.

### Suggested models

The suggestions come from Juiced's catalog and are chosen for this machine. For
each type, Juiced takes the largest recommended quant that fits a share of the
GPU memory, or of system memory when there is no GPU:

| Suggestion | Share of memory |
|---|---|
| First chat model | Half |
| Second chat model, embeddings, speech to text, text to speech, images | An eighth, at most 4 GiB |

A quant fits when its file size plus 15% is within the share. Models with a quant
already on disk or downloading are left out, as are gated models. The list is
chosen when Discover opens and does not reorder while a model downloads.

## A download is a model

A model has two actions: download it, or remove it. There is no step in between.

When a download finishes, Juiced picks the engine and hardware build for this
machine, installs that engine first when it is missing, and lists the model
under **Installed**. The engine install shows under **Downloads** too. The
download keeps its policy on the server, so reloading the page or restarting
Juiced does not lose it.

If the model can't be made, the **Downloads** row says why and offers **Retry**
and **Remove**. Juiced retries by itself when the cause can pass.

The same holds for files already on disk. Juiced lists every complete model
file it finds in its own storage or in a Hugging Face cache as a **Disabled**
model, named after the file. It does this at startup and while the console is
open, and it never changes a model that already exists. When a file can't
become a model yet, for example because its engine is not installed,
**Installed** says so in one line. Files that can never be a model, such as a
text encoder on its own, are listed only under
[Reclaim space](storage.md#reclaim-space).

See [choose loading policies](loading-policy.md) and
[runtimes and hardware](runtimes.md).

## Add an NPU model

On a machine with an AMD XDNA 2 NPU and FastFlowLM installed, Discover lists
FastFlowLM's chat models as results, marked **FastFlowLM · Chat · NPU**. Each
result is a model family. Its row offers one size, such as `4b`, and **N sizes**
opens the rest.
Pick **NPU** in **Any hardware** or **FastFlowLM (NPU)** in **Any engine** to see
only those.

1. Select **Download** on a result to fetch the size shown as **On demand**, or
   select the model's name to open the detail panel and pick another size.
2. In the detail panel, choose a **Loading policy**.
3. Select **Download**. The model is installed with that policy when the
   download finishes. A size that is already downloaded offers **Install**
   instead.

FastFlowLM downloads do not continue after a restart. If Juiced restarts during
one, download the model again.

## Download an original checkpoint

Some engines run a model's original folder instead of a GGUF file:

| Checkpoint | Engine | What Juiced downloads |
| --- | --- | --- |
| Transformers (`config.json` with `.safetensors` weights) | vLLM | Weights, shard index, config, generation config, tokenizer and chat template files |
| Exported ONNX classifier (`model.onnx`, `tokenizer.json`, `config.json`) | ONNX Runtime | The graph and its external data, tokenizer, vocabulary and config |
| Split diffusion files (`diffusion_models/`, `vae/`, `text_encoders/`) | TheNoise | One diffusion model with its VAE and text encoder |

1. Open **Models → Discover** and search, for example `Qwen3-0.6B`. Pick an engine
   in **Any engine** to see only what it runs. The **Classify** type lists text
   classifiers.
2. A checkpoint's row reads, for example, **Checkpoint · vLLM**. Select the
   model's name to see every file, the total size, the license and the engine. READMEs, images and
   other formats are skipped.
3. Choose a **Loading policy** and select **Download**. The files download at the
   repository's current commit and appear as one row under **Downloads**.
4. When the download finishes, Juiced names the model after the repository and
   installs it on the engine shown, installing that engine first if needed.

vLLM and TheNoise need a supported GPU. A checkpoint can still be downloaded on a
host without one, but its **Downloads** row then reports `no_usable_backend`.

Gated repositories need a Hugging Face token. Set `HF_TOKEN` on the server and
accept the repository's license on Hugging Face. Juiced sends the token only to
Hugging Face, never to the download mirrors it redirects to.

Files that two models share are stored once. Removing a model deletes only the
files no other model uses. See [manage storage](storage.md).

## Follow downloads

Open **Models → Downloads**. The tab lists only downloads in progress or failed,
and counts them. Model downloads and engine installs both appear here, with file
progress and errors. A finished download leaves the list by itself; its model is
under **Installed**.

- **Pause** stops a running transfer. **Resume** continues a paused or
  interrupted one, or a download that failed because the connection dropped,
  from the bytes already on disk. It still installs the model when done.
- **Retry** starts a failed transfer again from the beginning. On a row that
  reads **Not installed**, it tries to make the model again.
- **Cancel** stops a running download and deletes its partial files.
  **Remove** does the same for a paused or failed one, and also deletes its
  finished files when no model uses them.
- A split download is one row. It shows how many parts are on disk.
- A finished download shows **Installing** while Juiced makes its model.

Downloads continue after a restart from the bytes already on disk. Paused ones
stay paused. No download loads a model by itself.

## Download from a URL

To download one file from another site, select
**Add file → Download from a URL…**.

1. Enter the **Download URL**. It must be a direct link to the file, without a
   password, query or fragment.
2. Optionally enter the publisher's **SHA256**. Juiced then keeps the file only
   when it matches.
3. Select **Start download**.

Juiced reads the file size from the server. If the server does not report it,
the download may use the free space above the storage reserve, and stops when
the file is bigger. A file downloaded without a SHA256 shows **unverified** under
**Downloads** and **Unverified** in its file details. Add it like any local file.

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
5. Choose a **Loading policy** and select **Install**.

An uploaded or copied file that is left without a model for 30 seconds is
installed by Juiced as **Disabled**, like any other file on disk. A file used
in place is not.

A file used in place must stay at the same path. Moving or deleting it breaks
the model.

If Juiced says **This folder isn't one Juiced may import from.**, the file or a
link it follows is outside `import_roots`. Add the folder to `import_roots` in
the service config. **Juiced can't tell which engine runs this file.** means the
file is not a format Juiced reads. The (i) beside the message lists
the formats it reads.

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
title to sort. The first chips filter by service. **Enabled** and **Disabled**
choose what the list shows; Juiced shows Enabled models when there are any, and
a search also lists Disabled models that match. A model whose name does not say
which quant it runs shows the quant beside its service.

With a model open, press Escape to close it. The arrow keys move between its
tabs.

Every model picker in the console works the same way. Open it and type part of
a name, a type such as `chat`, `image` or `speech`, or a state such as `loaded`.
Models are listed as **Loaded**, **Enabled** and **Disabled**, with type and
memory beside each. In a long list, Disabled models wait behind **Show N
disabled**; a search lists them without asking. Use the arrow keys and Enter to
choose, or Escape to keep the current choice.

**Check for updates** compares each downloaded Hugging Face model with the
current upstream file and marks changed ones **update available**. The check
itself downloads nothing. Scripts use `POST /v1/models/check-updates`.
`/v1/models` then reports `update_available` per model until the service
restarts.

Select **Update** beside **update available**, in the list or in the model's
details. Juiced downloads the new files and moves the model to them. Its
settings and loading policy stay. A loaded model reloads on the new files.
Progress shows under **Downloads**. If the update fails, the model keeps its old
files. After a successful update Juiced deletes the old files unless another
model uses them. **Update** works for models whose files Juiced downloaded from Hugging
Face. For other models, download the new files from Discover.

When Juiced knows a model's repository, the model's details link to its model
card and README on Hugging Face.

## Remove a model

1. Open the model and unload it if it is loaded.
2. Open the **Manage** tab. It lists the model's files and where they are.
3. Select **Remove model**. The confirmation says how many files are deleted,
   the space that frees and which files stay.
4. Select **Remove model** again to confirm.

Removing a model deletes the files only it uses.

- A file another model uses stays until that model is removed too.
- A file you linked from your own folder stays where it is.
- **Remove model** is unavailable, with the reason beside it, when the model is
  loaded or busy, or when a file is in a read-only folder or in a Hugging Face
  cache outside Juiced's data folder. Juiced would find those files again and
  list the model again. Set the model to **Disabled** to keep its files and
  hide it from apps.

See [choose a published variant](variants.md) to add another quant while keeping
the original model.
