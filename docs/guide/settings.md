# Change model settings

Use a model's details to change execution and generation settings, then inspect
the values Juiced actually applies.

## Edit a model

1. Open the model and select **Settings**.
2. Change **Context**, **Run on** and the **Generation defaults**. Turn on
   **Advanced** for threads, GPU layers, prompt threads, batch, physical batch,
   flash attention, K and V cache precision, and **Extra arguments**. Each field
   has an (i) with its meaning.
3. Select **Save**. A loaded model shows **Save and reload** and restarts with
   the new settings.

An empty field shows what applies instead: `0.9 (profile)` is a value the
profile already passes to the engine, and `Default 0.8` is llama.cpp's default.
Total context is shared by the model's slots; clients read the tokens one request
can use from `context.per_request_tokens` in the model catalog, see
[Juiced management](../api/juiced.md#discover-models-for-a-client).

Generation defaults (temperature, top P, top K, min P, repetition penalty and
reasoning budget) apply when a request does not set its own value. A reasoning
budget of `0` turns thinking off; `-1` is unlimited.

**Extra arguments** are passed to llama-server after Juiced's own, for example
`--spec-type draft-mtp` or `--chat-template-kwargs '{"enable_thinking":false}'`.
Quote values that contain spaces. Flags that have their own field, and the host,
port, model, projector and cache paths, are refused.

Models adopted from Juiced 0.9 keep their `llamacpp_args`: typed flags such as
`--temp` show as the profile value, the rest appear under **Extra arguments**.
Per-backend arguments (`llamacpp.rocm_args`, `vulkan_args`, `cpu_args`, and the
same for sd-cpp and whisper.cpp) are used for the backend the host runs, unless
the model saved its own arguments.

### From a script

```bash
curl --fail-with-body http://localhost:8080/v1/models/your-model-id/options
curl --fail-with-body http://localhost:8080/v1/models/your-model-id/options \
  -H 'Content-Type: application/json' \
  -d '{"expected_revision":7,"options":{"context_tokens":32768,"temperature":0.6,"top_k":20,"flash_attention":"on","cache_type_k":"q8_0"},"extra_args":["--spec-type","draft-mtp"]}'
```

`GET` returns `saved` (the options you set), `effective_launch` (what the engine
starts with, including values from the profile's arguments), `defaults`
(llama.cpp's own) and `extra_args`. `options` replaces all typed options, so send
the full set; `extra_args` is optional and replaces the extra arguments. Option
keys: `context_tokens`, `threads`, `gpu_layers`, `parallel_slots`, `batch_size`,
`ubatch_size`, `batch_threads`, `flash_attention` (`auto`, `on`, `off`),
`cache_type_k` and `cache_type_v` (`f16`, `bf16`, `f32`, `q8_0`, `q5_1`, `q5_0`,
`q4_1`, `q4_0`, `iq4_nl`), `temperature`, `top_k`, `top_p`, `min_p`,
`repeat_penalty` and `reasoning_budget`. Add `"apply":"reload"` to restart a
loaded model. With access keys on, `POST` needs the admin key.

Settings of models adopted into a native settings store edit only the context.

### Copy settings to other quants

**Other quants** in **Settings** copies context, threads, GPU layers, slots,
batching, attention and KV cache, generation defaults and extra arguments to the
quants you tick. A setting that doesn't fit a quant stays as it is. Scripts can
limit the copy with `fields` (`context_tokens`, `threads`, `gpu_layers`,
`parallel_slots`, `batching`, `attention`, `sampling`, `extra_args`) on
`POST /v1/models/{model}/variants`.

## Set image defaults

An image model's defaults apply to every image request that leaves the field
unset, from any client. They are the image worker's launch settings, so they
take effect on the next load. To apply them now, use a reload.

```bash
curl --fail-with-body http://localhost:8080/v1/models/your-image-model-id/options
curl --fail-with-body http://localhost:8080/v1/models/your-image-model-id/options \
  -H 'Content-Type: application/json' \
  -d '{"expected_revision":7,"image_defaults":{"steps":8,"width":1024,"height":1024,"cfg_scale":null},"apply":"reload"}'
```

`GET` returns `image_defaults` with `steps`, `cfg_scale`, `width`, `height`,
`sampling_method`, `scheduler`, `flow_shift` and `seed`; `null` means the engine
default. `POST` changes only the keys you send, and `null` clears one. Use the
`revision` from `GET` as `expected_revision`. Without `"apply":"reload"`, unload
the model first. With access keys on, `POST` needs the admin key. Models adopted from Juiced 0.9 keep their `image_defaults` and
recipe options here.

## Set speech options

asr4all (FUTO) models have **Threads**, **Commit** and **Right context** in
**Settings**. From a script, send `asr_options` the same way as `image_defaults`:
`{"expected_revision":7,"asr_options":{"threads":4,"commit":null}}`.

## Return to defaults

Empty a field and save. An empty field is different from entering zero.

## Verify a change

Send a small representative request through **Test services** or your client.
Check the worker, effective configuration, error details, and output.
Compare one change at a time if you are investigating quality or speed.

[Cache mode](cache.md) controls computation reuse separately from model generation
settings. [Runtime profiles](runtimes.md) control the engine and hardware choice.
