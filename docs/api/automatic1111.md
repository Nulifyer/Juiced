# Use the AUTOMATIC1111 image API

Point a compatible text-to-image client at Juiced's server root, for example
`http://localhost:8080`. This interface supports Juiced's documented sd-cpp subset.

## Choose an image model

1. Download and enable a compatible sd-cpp image model.
2. Complete an Images test in Juiced.
3. List the image models:

```bash
curl --fail-with-body http://localhost:8080/sdapi/v1/sd-models
```

If several image models are enabled, supply `model` in the generation request
or select `sd_model_checkpoint` through the options subset.

## Generate an image

```bash
curl --fail-with-body http://localhost:8080/sdapi/v1/txt2img \
  -H 'Content-Type: application/json' \
  -d '{"model":"your-enabled-image-model-id","prompt":"A small wooden cabin in a snowy forest","width":512,"height":512}'
```

Replace the example model ID with one from the model list.
The response contains image data for the client to decode. Explicit supported
request values override model defaults. Size and sampling support depend on the
model and runtime.

## Supported subset

| Endpoint | Operation |
| --- | --- |
| `GET /sdapi/v1/sd-models` | Enabled compatible image models |
| `GET /sdapi/v1/options` | Read image-model selection |
| `POST /sdapi/v1/options` | Change only `sd_model_checkpoint` |
| `POST /sdapi/v1/txt2img` | Text-to-image generation |
| `GET /sdapi/v1/samplers` | Supported samplers |
| `GET /sdapi/v1/schedulers` | Supported schedulers |
| `GET /sdapi/v1/progress` | Current image-generation progress |

Image-to-image, scripts, file-based LoRAs, high-resolution passes, and global
interrupt are unavailable through this interface. A client requiring those
features needs another workflow. The same workers and loading policies serve
the OpenAI image API and Juiced's test page.
