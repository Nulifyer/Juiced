# Choose a loading policy

Set each downloaded model's policy to control when Juiced loads it and whether
clients can discover it.

| Policy | Behavior | Use it for |
| --- | --- | --- |
| Hot | Keeps the model loaded and attempts startup recovery | Services you want ready without a first-request load |
| On-demand | Allows client requests to load the model when needed | Models you use occasionally |
| Disabled | Excludes the model from client discovery and inference | Stored weights you are not serving |

## Change a policy

1. Open **Loading policy**, or open a model's details.
2. Select **Hot**, **On-demand**, or **Disabled**.
3. Watch the model's operation and worker state until the change settles.
4. Test the model if you have enabled a new service.

A downloaded model can be unloaded. An enabled model can also be waiting for
memory, a runtime package, or an accelerator. Check the state and error details
rather than treating its policy as proof of readiness.

## Load or unload a worker

Use the model's load/unload controls in **Models**. Loading starts its runtime
process. Unloading releases the worker's resources and preserves its weights.

Busy workers and protected Hot models can refuse operations that would interrupt
service. Wait for the request to finish, then review the policy and retry.
Do not repeatedly restart the server to force a runtime change.

## Fit several services on one host

Start with one small model. Add Hot services while watching actual memory in
**Diagnostics**. Allow room for context, runtime buffers, speech, image jobs,
and the host operating system.

The default container configuration limits loaded models to four. Changing a
count limit does not create memory or qualify a concurrent workload. Engines
also have different slot and device restrictions. NPU workers can require
exclusive accelerator access.

If a request waits or returns a capacity error, inspect the active workload
before increasing limits. See [troubleshooting](troubleshooting.md).
