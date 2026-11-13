# Change model settings

Use a model's details to change execution and generation settings, then inspect
the values Juiced actually applies.

## Edit a model

1. Open a downloaded model.
2. Select **Settings**, **Runtime**, or **Advanced** for the setting you need.
3. Change the value and read its contextual help.
4. Use the pane's save or apply action.
5. Complete any required reload once the worker is idle.
6. Open **Effective** to check the resulting value and its source.

Settings belong to the selected execution profile. Switching profiles can change
which saved settings apply. A pending setting is not yet active in the loaded
worker. Review unsaved changes before leaving a pane.

## Set context and generation defaults

Start with the model's defaults. Increase total context only when the workload
needs it and enough memory is available. Total context can be shared across
runtime slots. Do not assume the configured total is available independently
to every concurrent request. Clients read the tokens one request can use from
`context.per_request_tokens` in the model catalog; see
[Juiced management](../api/juiced.md#discover-models-for-a-client).

Temperature, token budget, and other generation defaults provide fallback values.
An explicit supported client parameter takes precedence. Runtime capability
checks and service resource limits still apply.

For images, width, height, steps, sampler, and scheduler support depends on the
selected model and engine. A larger image can require substantially more memory.
Use a temporary test override when comparing sizes.

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

## Return to defaults

Clear a saved override through the pane's default/reset control when available.
Then inspect **Effective** to confirm that the inherited value applies.
An empty or removed override is different from explicitly entering zero.

## Verify a change

Send a small representative request through **Test services** or your client.
Check the worker, effective configuration, error details, and output.
Compare one change at a time if you are investigating quality or speed.

[Cache mode](cache.md) controls computation reuse separately from model generation
settings. [Runtime profiles](runtimes.md) control the engine and hardware choice.
