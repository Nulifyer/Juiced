# Download or import models

Add weights through **Library**, then enable the model when you are ready to use it.
New downloads, imports, and prepared variants start **Disabled**.

## Download from the catalog

1. Open **Library → Discover models → Model catalog**.
2. Filter by service, capability, runtime, size, or download status.
3. Open a model to inspect its description and components.
4. Select **Download**.
5. Check progress under **Library → Downloads**.

**Recommended** is a catalog label. It does not set the model's Hot policy or
establish that the model fits your hardware.

## Find a GGUF on Hugging Face

1. Open the **Hugging Face** discovery tab.
2. Choose **llama.cpp · GGUF** as the target runtime.
3. Search by model name, or paste a Hugging Face model URL.
4. Open a repository and review its available quants.
5. Choose the quant and required companion files.
6. Start the download and check **Downloads** for completion or errors.

A quant is a weight representation such as `Q4_K_M` or `Q6_K`. Smaller quants
usually reduce storage and memory, with a precision tradeoff. Download size does
not include all memory needed to serve the model.

Search for vLLM or FastFlowLM candidates with the corresponding target runtime.
Use **Review files** to inspect the publisher's package. The GGUF downloader does
not import every format shown in those searches.

## Import a local GGUF

1. Open **Library → Import GGUF**.
2. Choose a local upload or a path visible to the server.
3. Supply the model information requested by the form.
4. Review the registration and finish the import.
5. Open the imported model and choose a compatible runtime.

A browser file selection uploads bytes to Juiced. A server path references files
already accessible to the server. For Docker, use a container path such as
`/library/model.gguf`, rather than the path on your laptop.

Vision models can require a separate projector. Check **Weights & variants** and
**Effective** for resolved components before enabling the model.

## Enable the model

1. Open **Models** or **Library → Downloaded**.
2. Open the model details.
3. Choose [Hot or On-demand](loading-policy.md).
4. Load it and [complete a service test](testing.md).

Keep the exact public model ID when configuring clients. Quant variants are
separate registrations and can have different runtime settings and loading policies.

## Remove a model

1. Open its details and select **Manage**.
2. Review the files and references shown in the removal flow.
3. Confirm the intended removal.
4. Check that the operation completes before reclaiming related storage.

Do not manually remove shared weights or files mounted from an existing library.
Other model registrations can use them. Preparation and runtime operations can
also hold references that block removal.

See [prepare another quant](preparation.md) to create a separate variant while
preserving the source.
