# Native MCP tool metadata

This generated reference records the serving core's tool schemas. It is not the
Juiced user guide. Start with [MCP setup](../docs/api/mcp.md).

Juiced policy filtering and runtime-only enforcement still apply, even when an
inherited schema describes upstream download or agent behavior. Inspect the
running release and the [capability contract](CAPABILITIES.md).

<!-- BEGIN GENERATED: mcp-tools -->
<!-- Generated from live /mcp tools/list. Do not edit this region by hand. -->

### `lemonade_list_models`

List models known to the Lemonade server. ALWAYS call this first if you don't already know the exact model name to use for chat/transcribe/image — passing a wrong name may trigger a multi-GB download. Returns a summary text block plus a JSON block with `{loaded, available, suggested_to_pull, recommended_chat_model}`.

**Returns**

Success returns two `text` content blocks: a human-readable summary, then a JSON-stringified object with `loaded`, `available`, `suggested_to_pull`, and `recommended_chat_model`.

| Argument | Required | Schema | Description |
|---|:---:|---|---|
| `include_available` | no | `{"type":"boolean"}` | Include downloaded models in the `available` list. |
| `include_suggested` | no | `{"type":"boolean"}` | Include suggested, not-yet-downloaded models in `suggested_to_pull`. |

### `lemonade_chat`

Chat completion against a locally hosted LLM. Pass a `messages` array (OpenAI chat format). `model` is OPTIONAL: when omitted, the server reuses an already-loaded LLM, else an already-downloaded one; if neither exists it asks you to either pass a `model` or `allow_download: true` (which downloads the default, Qwen3.5-4B-MTP-GGUF). Call `lemonade_list_models` first if you want to choose explicitly — a wrong name may trigger a multi-GB download.

**Returns**

Success returns one `text` content block with the assistant output. If the model emits tool calls, a second `text` block prefixed with `tool_calls: ` contains the JSON tool-call array. If normal content is empty but reasoning content is present, the first block contains the reasoning fallback instead of an empty string.

| Argument | Required | Schema | Description |
|---|:---:|---|---|
| `allow_download` | no | `{"type":"boolean"}` | Permit downloading the default model when none is loaded or downloaded. Defaults to false. |
| `chat_template_kwargs` | no | `{"type":"object"}` | e.g. {"enable_thinking": true} to enable reasoning blocks; disabled by default |
| `max_tokens` | no | `{"type":"integer"}` | Maximum number of output tokens. |
| `messages` | yes | `{"items":{"type":"object"},"type":"array"}` | Conversation messages in OpenAI chat format. |
| `model` | no | `{"type":"string"}` | Optional. Omit to auto-select a loaded/downloaded LLM; defaults to Qwen3.5-4B-MTP-GGUF only with allow_download=true. |
| `response_format` | no | `{"type":"object"}` | OpenAI-compatible response format configuration. |
| `seed` | no | `{"type":"integer"}` | Optional random seed for reproducible sampling. |
| `stop` | no | `{}` | stop sequences (string or array) |
| `temperature` | no | `{"type":"number"}` | Sampling temperature. |
| `tool_choice` | no | `{}` | auto \| none \| required \| {type: function, ...} |
| `tools` | no | `{"items":{"type":"object"},"type":"array"}` | OpenAI-compatible tool definitions available to the model. |
| `top_p` | no | `{"type":"number"}` | Nucleus sampling probability. |

### `lemonade_transcribe_audio`

Transcribe an audio clip with a Whisper-class model. The Lemonade MCP server always runs on the same machine as the caller, so prefer `audio_path` (an absolute path to a local audio file: wav, mp3, m4a, ogg, flac, webm). Use `audio_base64` only when you genuinely have audio bytes in memory. If both are provided, `audio_path` takes precedence. `model` is OPTIONAL: when omitted, the server reuses an already-loaded transcription model, else an already-downloaded one; if neither exists it asks you to pass a `model` or `allow_download: true` (which downloads the default, Whisper-Tiny).

**Returns**

Success returns two `text` content blocks: the transcript, then the JSON-stringified full transcription response, including timestamps or segments when provided by the backend.

| Argument | Required | Schema | Description |
|---|:---:|---|---|
| `allow_download` | no | `{"type":"boolean"}` | Permit downloading the default model when none is loaded or downloaded. Defaults to false. |
| `audio_base64` | no | `{"type":"string"}` | Base64-encoded audio data. Used when `audio_path` is not provided. |
| `audio_path` | no | `{"type":"string"}` | Path to a local audio file. Preferred over `audio_base64`; absolute paths avoid working-directory ambiguity. |
| `filename` | no | `{"type":"string"}` | Original audio filename, used as an input-format hint. |
| `language` | no | `{"type":"string"}` | Optional source language hint. |
| `model` | no | `{"type":"string"}` | Optional. Omit to auto-select a loaded/downloaded model; defaults to Whisper-Tiny only with allow_download=true. |
| `prompt` | no | `{"type":"string"}` | Optional text prompt to guide transcription. |
| `response_format` | no | `{"enum":["json","text","srt","verbose_json","vtt"],"type":"string"}` | Transcription response format. |
| `temperature` | no | `{"type":"number"}` | Sampling temperature for transcription. |

### `lemonade_generate_image`

Generate one or more images from a text prompt. The Lemonade MCP server always runs on the same machine as the caller, so PREFER writing the result directly to disk by passing `output_path` (single image) or `output_dir` (one or more). When you do, the tool returns absolute file path(s) as text — no base64 round-trip and dramatically fewer tokens. Only omit both arguments when you genuinely need the image inline. For safety, disk writes are confined to a sandbox directory (&lt;cache_dir&gt;/mcp-images, or LEMONADE_MCP_IMAGE_DIR if set); paths outside it are rejected. `output_dir` writes get unique auto-generated filenames (so concurrent callers never clobber each other); use `output_path` when you need an exact name. `model` is OPTIONAL: when omitted, the server reuses an already-loaded image model, else an already-downloaded one; if neither exists it asks you to pass a `model` or `allow_download: true` (which downloads the default, SD-Turbo).

**Returns**

Without `output_path` or `output_dir`, success returns one native MCP `image` content block per generated PNG. With disk output, success returns a summary `text` block, optional per-image path blocks when multiple files are written, and a final JSON-stringified `paths` object in a `text` block.

| Argument | Required | Schema | Description |
|---|:---:|---|---|
| `allow_download` | no | `{"type":"boolean"}` | Permit downloading the default model when none is loaded or downloaded. Defaults to false. |
| `cfg_scale` | no | `{"type":"number"}` | Classifier-free guidance scale. |
| `model` | no | `{"type":"string"}` | Optional. Omit to auto-select a loaded/downloaded image model; defaults to SD-Turbo only with allow_download=true. |
| `n` | no | `{"minimum":1,"type":"integer"}` | Number of images to generate. |
| `negative_prompt` | no | `{"type":"string"}` | Text describing content to avoid in the generated image. |
| `output_dir` | no | `{"type":"string"}` | Directory to write generated images into, inside the MCP image sandbox. Filenames are auto-generated and unique (image_{token}_{index}.png); the returned paths tell you the exact names. Relative paths resolve against the sandbox root; absolute paths must stay within it. |
| `output_path` | no | `{"type":"string"}` | Exact path of the PNG file to write, inside the MCP image sandbox (&lt;cache_dir&gt;/mcp-images or LEMONADE_MCP_IMAGE_DIR). Relative paths resolve against the sandbox root; absolute paths must stay within it. Written as named (overwrites if it already exists). Only valid when n == 1. |
| `prompt` | yes | `{"type":"string"}` | Text prompt describing the image to generate. |
| `seed` | no | `{"type":"integer"}` | Optional random seed for reproducible generation. |
| `size` | no | `{"type":"string"}` | Requested image size as WIDTHxHEIGHT, for example 512x512. |
| `steps` | no | `{"type":"integer"}` | Number of diffusion sampling steps. |

### `lemonade_docs`

Read this server's own API reference. Call with no arguments to list the pages it ships, then pass `page` (an `id` from that list) to read one as markdown. The docs are bundled with the server, so they describe the version actually running and work offline. Use this before hand-writing requests against Lemonade endpoints.

**Returns**

With `page`, success returns one `text` block containing the raw bundled Markdown page. With no arguments, success normally returns a summary `text` block followed by a JSON-stringified `pages` object containing `id`, `title`, and `bytes` for each page; if no bundled documentation is installed, it returns one explanatory `text` block instead. Unknown pages return `isError: true` with one explanatory `text` block. The same bundled documentation is also available through `GET /v1/docs`.

| Argument | Required | Schema | Description |
|---|:---:|---|---|
| `page` | no | `{"type":"string"}` | Optional. Page id from the listing, e.g. 'api/lemonade'. Omit to list. |

<!-- END GENERATED: mcp-tools -->
