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
