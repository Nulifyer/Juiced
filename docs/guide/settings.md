# Change model settings

Use a model's details to change execution and generation settings, then inspect
the values Juiced actually applies.

## Edit a model

1. Open the model and select **Settings**.
2. Change **Context**, **Slots**, **Run on** and the **Generation defaults**.
   Turn on **Advanced** for threads, GPU layers, prompt threads, batch, physical
   batch, flash attention, K and V cache precision, and **Extra arguments**. The
   (i) next to a field or group explains it.
3. Select **Save**. A loaded model shows **Save and reload** and restarts with
   the new settings.

New models start at a 32K context, or the trained size when that is smaller.
Under **Context**, **Auto** fills in the largest context that fits the memory
budget and **Max** the size the model was trained for. The line below the field
shows the maximum, and how much fits when that is less. A larger context reserves
more memory whether or not a request uses it.

**Slots** is how many requests the model serves at once. The slots share the
context, so each request gets a smaller part of it, and the memory estimate
updates as you type. Empty uses the slots in the model's launch arguments.

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
port, model, projector and cache paths, are refused. Speculative decoding takes
`--spec-type` with its `--spec-ngram-*` sizes and `--spec-draft-n-max` /
`--spec-draft-n-min`; a separate draft model (`--spec-draft-model` and the other
`--spec-draft-*` flags) is refused.

Models adopted from Juiced 0.9 keep their `llamacpp_args`: typed flags such as
`--temp` show as the profile value, the rest appear under **Extra arguments**.
Per-backend arguments (`llamacpp.rocm_args`, `vulkan_args`, `cpu_args`, and the
same for sd-cpp and whisper.cpp) are used for the backend the host runs, unless
the model saved its own arguments.

## Check a model

**Check** on a model's Overview sends a few short real requests: a chat reply,
a tool call, the thinking switch and an image, for the features the model
declares, and shows its decode speed. Each line passes, fails or is skipped with
a short reason; hover it for the full text. The chat line shows how long the
reply took. Juiced runs it by itself after a chat model starts with new settings
or a new engine; **Run check** runs it again, and **Load and check** loads an On
demand model first.

## Juiced defaults

**Juiced defaults** in Settings lists what Juiced adds when the model starts and
where each argument comes from: a tested recipe for that model, something its file
declares (such as an MTP head for faster decoding), or native Juiced's default for
its model family. Juiced works these out at every start, so a newer release
improves existing models on their next load. When a later release adds an
argument this browser has not seen for the model, Settings says so once, with
**OK** and **Turn off**. The first time you open the model nothing is shown. Your own settings and
extra arguments always win. Turn the switch off to start the model with only your
settings; a loaded model restarts.

**Tune for this machine** tries a larger prompt batch, flash attention and
speculative decoding, measuring prompt, chat and code-rewrite speed, and keeps a
change only when it is clearly faster. Tune also checks the answers: it asks the
same questions at temperature 0 with every setting, and a change that gets one
wrong that the current settings got right is not kept, however fast it is. The
results table marks those answers. If a kept change writes noticeably different
text, Tune says so when it finishes. It reloads the model several times and
takes a few minutes. It never changes your saved settings, and it skips any
option you have set yourself. Tune needs Juiced defaults turned on, because its
results apply through them. Kept changes show as **Measured here**; **Clear tuned**
removes them. Results apply only to the engine version Tune measured. After an
engine change, Settings says so and the model starts without them until you
tune again.

Empty generation fields show the value that applies, such as `1 (model file)`
for a model whose file names its publisher's temperature.

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

An image model's **Settings** tab has **Image defaults**: steps, CFG scale,
width, height, sampler, scheduler, flow shift and seed. They apply to every image
request that leaves the field unset, from any client. They are the image worker's
launch settings, so they take effect on the next load. A loaded model shows
**Save and reload**.

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

With **Advanced** on, **Setting sources** lists each setting's **Saved** and
**Running** value and where it came from. **restart** marks a saved value that the
running engine does not use yet.

Send a small representative request through **Test services** or your client.
Check the model's state, any error, and the output. Compare one change at a time
if you are investigating quality or speed.

The [conversation cache](cache.md) controls prompt reuse separately from these
settings. [Runtimes and hardware](runtimes.md) covers the engine choice.
