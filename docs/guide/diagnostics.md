# Inspect runtime activity

Use **Dashboard** to check what is running now. Use **Diagnostics** to find out
how requests performed and why. Use the same model and workload when comparing
settings.

## Check the Dashboard

The Dashboard answers one question: is everything working right now?

- **Protocols** counts the client protocols that are ready and names those in use.
- **Requests** shows active and queued requests against the loaded request slots.
- **Model memory** shows memory reserved by loaded models against the admission
  budget, and how much is free for another model.
- **GPU memory** shows the first GPU's memory and utilization. An integrated GPU, such as a Radeon 890M, keeps models in shared system memory, so the tile adds the shared allowance (GTT) to the reserved carve-out. `/v1/system-stats` reports both: `memory_*_bytes` for the carve-out and `shared_memory_*_bytes` for shared memory, with `integrated: true`. Shared memory is the same RAM that system memory counts, so never add the two.
- **Running** lists loaded and starting models with their busy slots and when
  each was last used, or when an idle On demand model unloads.
- **Needs attention** lists failed models, failing protocol self-checks, requests
  that failed in the last hour, and an unavailable snapshot cache. Select an entry
  to open the model, protocol, or request list.

The footer shows current CPU, RAM, GPU, VRAM, and NPU use on every page.

## Read performance

1. Open **Diagnostics → Performance**.
2. Send a representative request.
3. Read the model's row under **By model**:
   - **Typical time** is the median duration of completed requests.
   - **Prompt time** compares reused prompts with prompts read in full. A reused
     prompt was still loaded in the engine or restored from disk, so only its new
     part was processed.
   - **Cached** is the share of prompt tokens served from the prompt cache.
   - **Speed** shows generated tokens per second, with prompt tokens per second below.
4. Check the CPU, memory, GPU, and VRAM charts for the last two minutes.

**Reservations** lists the declared limits used to admit work.
Missing or stale measurements remain unknown. A GPU's shared allocation can
overlap host RAM, so adding the two values can double-count memory.
Aggregate counters do not establish one worker's cache use or available context.

## Inspect requests

1. Open **Diagnostics → Requests**.
2. Find the request by its completion time and model.
3. Read its outcome, prompt and output tokens, and speed.
4. Use the **Duration** bar to see where the time went: queue, prompt, generate,
   and other work. Bars share one scale, so a slow request stands out.

The prompt column says whether the prompt was still loaded, restored from disk,
or processed in full, and how many tokens came from the cache. A long prompt
processed in full usually explains a slow first reply.

Prompt and generate times come from the engine's token rates. Other covers
restoring a saved conversation, audio work, and network time. Requests appear
after completion. The service keeps the latest 128 records. It records
operational metadata, never prompts or answer content. Missing token usage is
not inferred from another request.

## Read logs

1. Open **Diagnostics → Logs**.
2. Search, or choose a level and a source, to narrow the lines.
3. Select **Pause** to stop following new lines while you read.
4. Check the worker error alongside the model's current state.

Buffered warnings can remain after the original condition clears. Check the
current worker state before treating an old warning as an active failure.

## Read container logs

Service and worker lines also go to the container's standard error, so
`docker logs --tail 100 juiced` shows them. Each line looks like
`2026-11-12 14:22:05.456 [Info] (stories) message`, with a UTC timestamp and the
model ID, or `service`, in parentheses.

Choose how much reaches the container log with `log_level` in the configuration
or the `LEMONADE_LOG_LEVEL` environment variable, which wins. Use `trace`,
`debug`, `info` (the default), `warning`, `error`, `fatal` or `none`. Worker lines
without a level of their own count as `info`. The level does not filter
**Diagnostics → Logs**, which keeps every line. Restart the service after a change.

## Scrape metrics

`GET /metrics` returns Prometheus text with the native metric names:
`lemonade_server_up`, `lemonade_loaded_models`, per-model `lemonade_model_*`
gauges and counters labelled by `model_name`, totals such as
`lemonade_requests_total`, and `lemonade_cpu_usage_percent`,
`lemonade_memory_used_gb`, `lemonade_gpu_usage_percent`, `lemonade_vram_used_gb`
and `lemonade_npu_usage_percent`. Counters start at zero when the service starts.
It needs the same key as other read routes:

```yaml
scrape_configs:
  - job_name: juiced
    authorization:
      credentials: "<LEMONADE_API_KEY>"
    static_configs:
      - targets: ["juiced.example:8080"]
```

`lemonade_max_loaded_models`, `lemonade_model_time_to_first_token_seconds`,
`lemonade_routing_*` and the `lemonade_llamacpp_*` engine metrics are not exposed.

## Stream logs

Log tail clients can open the WebSocket `/logs/stream`, or `/v1/logs/stream`, with
the read key. Send `{"type":"logs.subscribe","after_seq":null}` to receive one
`logs.snapshot` of the retained lines, then a `logs.entry` for each new line. Send
the last `seq` you saw as `after_seq` to resume after a reconnect.

Remove credentials, private paths, prompts, and client identifiers before sharing
log excerpts. See [troubleshooting](troubleshooting.md) for specific failures.
