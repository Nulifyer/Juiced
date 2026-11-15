# Choose a loading policy

Set each model's policy to control when Juiced loads it and whether clients can
discover it.

| Policy | Behavior | Use it for |
| --- | --- | --- |
| Hot | Loads at startup and stays loaded | Services you want ready without a first-request load |
| On demand | Loads on the first request and can unload when idle | Models you use occasionally |
| Disabled | Unloads the model and hides it from apps | Stored weights you are not serving |

## Change a policy

1. Open **Loading policy**. The page has one column for each policy.
2. Drag a model to another column, or choose the policy in the model's menu.
   You can also change it on the model's **Overview** tab under **Models**.
   The change applies at once.
3. Watch the model's state until the change settles.
4. Test the model if you enabled a new service.

Before you make a model Hot, Juiced shows its reservation and the free model
memory, such as `Hot keeps 51.1 GiB loaded · 21.0 GiB free`. If it does not fit,
idle On demand models unload to make room. If Hot or busy models hold the rest,
it does not load.

An enabled model can also be waiting for memory, an engine, or an accelerator.
Check its state and error rather than treating its policy as proof of readiness.

## Unload idle models

**Unload when idle** in the **On demand** column sets how long an unused On demand
model stays loaded, from **never** to **after 4 h**. A model's **Overview** tab has
its own **Unload when idle** setting, which starts at **Default**. A client that
sends Ollama `keep_alive` overrides it until the model's next request.

## Load or unload a worker

Open the model under **Models**. On the **Overview** tab, select **Load worker**
or **Unload** under **Worker**. Loading starts the engine process. Unloading
releases its memory and keeps its weights. **Stop waiting** stops the console
from waiting for a slow load.

To stop a Hot model, set it to Disabled. That unloads it in one step. A model
with running requests cannot be disabled. Wait for the requests to finish, then
retry. Do not restart the server repeatedly to force a change.

## Fit several services on one host

**Model memory** in the **Loading policy** toolbar is the memory that loaded
models may reserve together. The container sets it to three quarters of the
host or container memory limit on first start. Select it to change the value
in GiB. When a load would go over it, idle On demand models unload first. Hot
and busy models stay.

Start with one small model. Add Hot services while you watch **Model memory**
and **GPU memory** on the Dashboard. The **Hot** column shows the same figure,
such as `59.4 of 80.0 GiB used`. Leave room for context, engine buffers, speech,
image jobs, and the host operating system.

Engines also have their own slot and device limits. NPU workers can need
exclusive access to the accelerator.

If a request waits or returns a capacity error, check the active workload
before raising limits. See [troubleshooting](troubleshooting.md).

There is no default per-user conversation quota. The engine's slots, the memory
budget and the shared request queue limit active work. A one-slot engine can
serve many conversations in turn. More slots need more context memory.
