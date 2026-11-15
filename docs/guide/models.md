# Download or import models

Add weights through **Library**, then enable the model when you are ready to use it.
New downloads and imports start **Disabled**.

## Find and download a model

1. Open **Library → Find & run models**.
2. Search the known models by name. Use **Filter known models** for service,
   capability, runtime, size or availability. Running and downloaded models appear first.
3. For a known model, select **Review download** to inspect its declared files,
   companions and size before selecting **Download model**.
4. To search published repositories with the same query, select **Search published
   models**. You can also paste a Hugging Face repository URL or `publisher/repository`.
5. Select **Choose files** on a repository, then explicitly choose a published quant
   and any required projector. Review the exact filenames and registration name.
6. Start the download. Activity opens on the same page; the query remains available.

**Catalog picks** identifies catalog labels. It does not establish host fit or
set the model's Hot policy. Download size excludes context, engine workspace and
other runtime allocations. Missing size or context metadata is reported as unknown.

A quant is a published weight representation such as `Q4_K_M` or `Q6_K`.
Smaller quants usually reduce storage and memory, with a precision tradeoff.
Juiced downloads published variants; it does not convert or quantize weights.
A vision model may require a matching projector. Selecting text only does not
qualify vision service.

## Download an original checkpoint

Some engines run a model's original folder instead of a GGUF file:

| Checkpoint | Engine | What Juiced downloads |
| --- | --- | --- |
| Transformers (`config.json` with `.safetensors` weights) | vLLM | Weights, shard index, config, generation config, tokenizer and chat template files |
| Exported ONNX classifier (`model.onnx`, `tokenizer.json`, `config.json`) | ONNX Runtime | The graph and its external data, tokenizer, vocabulary and config |
| Split diffusion files (`diffusion_models/`, `vae/`, `text_encoders/`) | TheNoise | One diffusion model with its VAE and text encoder |

1. Open **Models → Discover** and search, for example `Qwen3-0.6B`. Pick an engine
   in **Any engine** to see only what it runs; the **Classify** task lists text
   classifiers.
2. A checkpoint shows as one chip, such as **Checkpoint · vLLM**. Select it to see
   every file, the total size, the license and the engine. READMEs, images and
   other formats are skipped.
3. Select **Download**. The files download at the repository's current commit and
   appear as one row in **Downloads**, where you can pause, resume or cancel them.
   A restart continues from the bytes already on disk.
4. When the chip shows **✓ downloaded**, select **Add model**. Juiced names the model
   after the repository and registers it on the engine shown. If that engine is not
   installed, **Install engine and add** installs it first.

vLLM and TheNoise need a supported GPU. A checkpoint can still be downloaded on a
host without one; adding it reports `no_usable_backend`.

Gated repositories need a Hugging Face token. Set `HF_TOKEN` on the server and
accept the repository's license on Hugging Face; Juiced sends the token only to
Hugging Face, never to the download mirrors it redirects to.

Files from the same checkpoint that two models share are stored once. Removing a
model deletes only the files no other model uses. See [manage storage](storage.md).

## Follow progress and recover

Download activity stays inside **Find & run models**. The top-bar download button
opens the same activity. It shows file progress, retry delays and actionable errors.
Use **Pause** or **Cancel** to stop a transfer; wait for the worker to acknowledge
that stop before **Resume / retry** becomes available. Retry uses the retained
registration and source. Downloads continue after a restart from the bytes already
on disk; paused ones stay paused. See [manage storage](storage.md).

A finished download in **Downloads** offers **Add model**, which opens its file
details, or **Open model** once a model uses the files. Repeated downloads of the
same file show as one row. Downloading never enables the model automatically.

## Import a local GGUF

1. Open **Models → Discover** and select **Add file**.
2. Select **Upload from this computer** to send files to Juiced, or **Pick from the
   server** to use files the server can already read under `import_roots`. For
   Docker, that is a container path such as `/library`, not the path on your laptop.
3. Check the model name and, for a vision model, the projector. Then select
   **Add model**. If the engine is missing, **Install engine and add** installs it first.

A split GGUF keeps its parts as files named `Model-00001-of-00003.gguf`,
`Model-00002-of-00003.gguf` and so on. Keep those names, because llama.cpp finds
the other parts from the first one's name.

- To upload a split model, select every part in the file picker at once. Juiced
  checks that no part is missing before it uploads anything, then uploads the parts
  one at a time. If an upload fails partway, the parts that finished stay in the
  library. Pick the same files again to finish.
- On the server, the folder shows a split model once, as its first part with the part
  count and total size. Juiced uses split models where they are, so pick
  **Where it is**. If a part is missing, Juiced names it and adds nothing.

A vision model needs its projector, usually a file named `mmproj-…gguf`.

- When uploading, select the projector together with the model file or parts.
- When picking from the server, Juiced offers the projectors in the model's folder
  under **Vision projector** and selects the first one. Choose **None (text only)** to
  add the model without image input.

Uploading or picking only a projector is refused, because it cannot run alone. The
memory estimate counts every part and the projector.

## Enable the model

Open the model and select **Host fit → Check saved settings** before loading.
The result separates declared memory limits from current capacity and runtime
requirements. Expand **Memory details** to see the physical pools and reservations.
An unknown result needs an execution-specific estimate; weight size alone does
not establish fit. Save draft changes before checking them. A successful check
reserves no capacity and does not enable the model or qualify its output quality.

1. Open **Models** or **Library → Find & run models**.
2. Open the model details.
3. Review **Runtime** and **Settings**, then choose [Hot or On-demand](loading-policy.md).
4. Load it and [complete a service test](testing.md). A running model also offers **Try model** in Library.

Keep the exact public model ID when configuring clients. Quant variants are
separate registrations and can have different runtime settings and loading policies.

## Work with several models

Tick models in **Models → Installed** to load, unload or set the loading policy
of all of them at once. Select a column title to sort by name, state or memory.

**Check for updates** compares each downloaded Hugging Face model with the
current upstream file and marks changed ones **update available**. It downloads
nothing. Scripts use `POST /v1/models/check-updates`; `/v1/models` then reports
`update_available` per model until the service restarts.

When Juiced knows a model's repository, its details link to the model card and
README on Hugging Face.

## Remove a model

1. Open its details and select **Manage**.
2. Review the files and references shown in the removal flow.
3. Confirm the intended removal.
4. Check that the operation completes before reclaiming related storage.

Do not manually remove shared weights or files mounted from an existing library.
Other model registrations can use them. Unresolved legacy writers and runtime operations can
also hold references that block removal.

See [choose a published variant](variants.md) to acquire another compatible quant
while preserving the original model.
