# Native management API compatibility

Juiced retains native command and route compatibility from its serving core.
For current operator instructions, use [Juiced management](juiced.md).
For client requests, use the [API guide](README.md).

This page remains at its original path because native clients can link to it.
Juiced's current guides and native contracts define its supported behavior.

## Backend tiers and formats

Backend descriptors distinguish native, experimental, and guest implementations,
and record the artifact formats each importer accepts. These declarations do not
qualify every model or hardware combination. Check the selected profile in
Juiced's Runtime pane and complete an inference test.

See [runtimes and hardware](../guide/runtimes.md) for user instructions and
[generated backend metadata](https://github.com/Nulifyer/Juiced/blob/main/juiced/BACKENDS.md) for contributor reference.
