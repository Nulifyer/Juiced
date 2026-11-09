# Inspect runtime activity

Open **Diagnostics** to check performance, completed requests, and logs.
Use the same model and workload when comparing settings.

## Read performance

1. Open **Diagnostics → Performance**.
2. Check host memory and detected accelerator measurements.
3. Send a representative request.
4. Inspect the last completed inference and available token timings.

The footer also reports current runtime measurements. Select **Details** for
additional values when that control is available in your release.
Missing or stale measurements remain unknown. A GPU's shared allocation can
overlap host RAM, so adding the two values can double-count memory.
Aggregate counters do not establish one worker's cache use or available context.

## Inspect requests

1. Open **Diagnostics → Requests**.
2. Find the completed request using the available filters.
3. Inspect its protocol, model, status, duration, and recorded usage.
4. Open the associated model if its runtime needs investigation.

Requests appear after completion. The inventory retains a bounded recent history
of up to 200 request records. It records operational metadata rather than prompts
or answer content. Missing token usage is not inferred from another request.
A successful request establishes that operation for that model/profile, not every
feature advertised by the model publisher.

## Read logs

1. Open **Diagnostics → Logs**.
2. Choose the raw or parsed view for the information you need.
3. Narrow the visible entries around the failed operation.
4. Check the worker error alongside the management operation state.

Buffered warnings can remain after the original condition clears. Check the
current worker state before treating an old warning as an active failure.
For startup failures, use `docker logs --tail 100 juiced` from the host.

Remove credentials, private paths, prompts, and client identifiers before sharing
log excerpts. See [troubleshooting](troubleshooting.md) for specific failures.
