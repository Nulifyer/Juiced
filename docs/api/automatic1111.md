# Use the AUTOMATIC1111 image API

Point a compatible text-to-image client, such as SillyTavern's "SD WebUI (AUTOMATIC1111)"
source, at Juiced's server root, for example `http://localhost:8080`. This interface
supports Juiced's documented sd-cpp subset.

## Choose an image model

1. Download and enable a compatible sd-cpp image model.
2. Complete an Images test in Juiced.
3. List the image models:

```bash
curl --fail-with-body http://localhost:8080/sdapi/v1/sd-models
```

With one enabled image model, clients can omit the model. With several, supply
`model`, set `override_settings.sd_model_checkpoint`, or select one with
`POST /sdapi/v1/options`. Juiced keeps that selection until it restarts.

## Generate an image

```bash
curl --fail-with-body http://localhost:8080/sdapi/v1/txt2img \
  -H 'Content-Type: application/json' \
  -d '{"model":"your-enabled-image-model-id","prompt":"A small wooden cabin in a snowy forest","width":512,"height":512}'
```

Replace the example model ID with one from the model list.
The response contains `images` for the client to decode. `batch_size` can be 1 to 4.
Explicit request values override the model's [image defaults](../guide/settings.md#set-image-defaults).

Juiced accepts and ignores the fields A1111 clients send for saving and
high-resolution passes (`save_images`, `hr_*`, `denoising_strength`,
`override_settings_restore_afterwards`). It rejects `enable_hr: true`,
`restore_faces: true`, scripts such as ADetailer, and `n_iter` above 1.

## Supported subset

| Endpoint | Operation |
| --- | --- |
| `GET /sdapi/v1/sd-models` | Enabled compatible image models |
| `GET /sdapi/v1/options` | Read the image-model selection (`sd_model_checkpoint`) |
| `POST /sdapi/v1/options` | Change only `sd_model_checkpoint` |
| `POST /sdapi/v1/txt2img` | Text-to-image generation |
| `GET /sdapi/v1/samplers` | The engine's samplers; only `default` until the model loads |
| `GET /sdapi/v1/schedulers` | The engine's schedulers; only `default` until the model loads |
| `GET /sdapi/v1/progress` | Image-generation progress; idle while the model is unloaded |
| `GET /sdapi/v1/upscalers`, `sd-vae`, `latent-upscale-modes` | Always an empty list |
| `POST /sdapi/v1/img2img`, `POST /sdapi/v1/interrupt` | `501 Not Implemented` |

Read routes never load a model. Selection problems (no enabled image model,
several without a selection, an unknown or disabled model) return `409`.
Image-to-image, scripts, file-based LoRAs, high-resolution passes, and global
interrupt are unavailable through this interface. For image editing, use the
OpenAI [`/v1/images/edits`](openai.md#edit-images) route. The same workers and
loading policies serve the OpenAI image API and Juiced's test page.
