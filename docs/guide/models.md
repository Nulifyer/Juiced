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

The runtime filter also finds vLLM and FastFlowLM candidates. **Review files** opens
the publisher's package; this GGUF download flow does not import every format in
those results. Review runtime compatibility before acquiring a different format.

## Follow progress and recover

Download activity stays inside **Find & run models**. The top-bar download button
opens the same activity. It shows file progress, retry delays and actionable errors.
Use **Pause** or **Cancel** to stop a transfer; wait for the worker to acknowledge
that stop before **Resume / retry** becomes available. Retry uses the retained
registration and source. Interrupted transfers remain recoverable after restart.

After completion, select **Review model** or find the downloaded row in the same
search. Downloading never enables the model automatically.

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
