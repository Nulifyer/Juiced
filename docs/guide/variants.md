# Choose a published model variant

Juiced downloads or imports existing weights. It does not convert or quantize
models. A model family can have several repositories, formats and numerical
variants. Check what each candidate contains before downloading.

1. Open **Library** and search for the model family or publisher.
2. Review the repository and revision. Similar model names do not prove common
   provenance, licensing or equivalent behavior.
3. Inspect format, architecture and quantization. Choose a variant compatible
   with an installed consuming runtime, or review that runtime's prerequisites.
4. Include required shards, tokenizer/template and modality companions. A GGUF
   text model with a separate projector needs that companion for vision.
5. Check storage and physical memory separately. Whole weights, context/state,
   workspace and current services use memory. Active MoE parameters alone do not
   describe how many weights must fit. Treat an unknown estimate as unknown.
6. Download the selected variant or import supported existing local files.
   Inspect progress and errors. Keep the original model when adding another quant.
7. New acquisitions start **Disabled**. Explicitly select **On demand** or **Hot**
   before running the variant. Review its normal settings and runtime profile.
8. Use **Test services** for a temporary functional check, then connect your
   client through **Service APIs**. Actual load or inference failure needs its
   concrete runtime/resource diagnosis, rather than another local conversion.

For a loaded model, discovery separates declared, configured, applied and observed
context. An engine can cap the requested setting. See the effective settings
inspector and preserve unknown limits rather than assuming a large context works.

A higher-bit quant is not automatically a better choice on this host. Compare
compatible variants on the same tasks, runtime, context and resource conditions.
Juiced's versioned performance/quality comparison runner and the complete unified
Library suitability flow remain planned work. Current controls and publisher
claims alone do not qualify quality, fit or concurrency.

Existing weights from earlier conversion jobs remain usable and reference-protected.
See [retained conversion records](preparation.md) and [cache storage](cache.md).
