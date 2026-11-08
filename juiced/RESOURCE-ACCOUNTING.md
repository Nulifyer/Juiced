# Resource accounting

`GET /v1/system-stats` includes `memory_budget`. All four native API prefixes
expose the same contract. Existing CPU, GPU, RAM and inference fields remain.

This is a sampled memory bound before reservations. It does not qualify a model's
fit, promise admission or protect service latency. `reservation_status` is
`not-reserved` until shared admission is implemented and qualified.

## Physical memory and constraints

`host_total_bytes` and `host_available_bytes` describe one host RAM pool. Linux
reads `MemTotal` and `MemAvailable` in `/proc/meminfo`. Missing or invalid readings
remain null. `MemFree` is not a substitute for `MemAvailable`.

`constraints` lists the current visible memory-control group and its enforcing
ancestors. Each finite limit is paired with that group's own charged memory.
`available_bytes` is the minimum of host availability and each finite group's
remaining headroom. These are overlapping constraints, not memory capacities to
add together. Reaching or exceeding a limit produces zero, not unsigned overflow.

The Linux reader resolves `/proc/self/cgroup` against `/proc/self/mountinfo`.
It supports cgroup v2 and the v1 memory controller. V1 ancestor constraints apply
only where hierarchical accounting is enabled. Unlimited is distinct from unknown.
An unreadable finite constraint or an unresolved mount makes effective availability
null. Missing root interfaces are accepted only at the visible hierarchy root
when both memory files are genuinely absent and the memory controller is visible.
An empty, malformed or unreadable interface is never treated as root-file absence.
No shell command, host repair or driver installation occurs.

`visible_constraints_complete` covers the mounted hierarchy only. A container
namespace can hide ancestor limits. `hidden_ancestor_limits` remains `unobserved`.
Non-Linux platforms currently expose their available host estimate, with container
constraints unknown. This contract does not qualify Windows job limits, macOS
containers or unseen hardware.

## GPU and NPU memory

A GPU's `physical_pool` is `host-ram` when native detection identifies shared
memory. A confirmed dedicated GPU has its own `device:<id>` pool. Unknown memory
relationships remain null. Multiple shared GPU views reference the same host pool.
Do not add GPU shared allocation to host RAM usage or add their capacities.

Driver allocations and cgroup charges have different coverage. On some hardware,
large GPU allocations do not appear in `memory.current`. The contract therefore
reports `charged_memory_covers_device_allocations: unverified`. Container headroom
alone cannot establish GPU or NPU fit. Model weights, runtime workspaces, KV state
and XRT allocations still need their own bounded estimates and shared reservations.

Linux AMD utilization identifies `sysfs-gpu_busy_percent` as its source. The driver
may report activity even when no Juiced request is active. Juiced preserves the
reading rather than replacing it with an invented idle value. Other clients and
driver behavior can also affect hardware counters.

## Automatic context sizing

Linux automatic CPU/NPU context sizing uses this host and container bound. Shared
GPU sizing caps its existing device-aware estimate by the same bound. It no longer
uses a GPU's virtual mapping capacity as a proxy for CPU RAM on Linux. Unknown
readings retain the existing conservative context fallback.

Explicit saved context sizes and existing loaded workers are unchanged. This does
not make context sizing an atomic resource reservation. Shared load/request leases,
bounded queues, maintenance drain and essential-service interference acceptance
remain the rest of platform phase P1.

## Worker ownership

Router inference, streaming, tokenization and raw slot operations hold a scoped
worker reference. Success, exceptions and retry paths release it once. Watchdog
recovery relinquishes that reference before replacing the worker. Telemetry copies
its URL/parser while the worker is owned. A failed sleep restore returns to the
downsized state and wakes waiters instead of leaving the worker stuck loading.

Managed worker references retain the exact worker generation even after registry
ownership is removed. Cache restore/save holds a separate state-operation reference
through JSON/SSE finalization. It blocks eviction without adding another inference
request to the active-call counter. Explicit unload waits for references outside
Router's lifecycle lock, then revalidates the selected generations. The work-drain
wait is limited to five seconds; later slot waits and process teardown are separate
lifecycle work. A timeout preserves the worker. Automatic replacement rejects busy
workers rather than waiting for their release while holding the global lock.

This worker reference prevents eviction during native calls. Cancellation
acknowledgement or completion of a proxy transfer does not prove that a device
kernel has stopped. Future resource reservations must retain or reconcile that
separate lifetime before reusing its budget.

`stats.live.active_requests` counts native worker calls, including prefill, speech
and images. `generating_requests` retains the narrower recent text-output count.
The throughput estimate counts output only. Prefill must not invent token output
or disappear from the active-work counter. Direct realtime backend sessions hold
one worker reference per connected TCP session, including periods awaiting audio.
Their address and worker generation are acquired together through Router without
waiting for restoration under its lifecycle lock. Model changes close the old
connection before borrowing the new worker. Failed connections and session close
release the reference; manager shutdown closes sessions before waiting for pending
transcriptions. These references prevent unloading a connected worker, not reserve
memory or prove kernel termination. Cache save/restore lifetime integration and
shared admission remain unfinished. A zero text-output count does not establish
idle hardware.
