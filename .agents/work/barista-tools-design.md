# Barista tool design (primary agent, 2026-11-12)

Goal from the user: start one model by hand, then let it set up the rest of Juiced. Barista must be able to finish real setup jobs end to end: acquire and run models that fit, wire clients, tune, diagnose, clean up. Contract: juiced/BARISTA.md rev 2 (tool groups Inspect, Configure, Acquire, Maintain, Compare, Explain; skills; authority rechecked per dispatch; no shell; no conversion/quantization).

## Jobs Barista must complete (acceptance stories)

1. "Set this box up for Home Assistant voice": check hardware, pick and install STT + TTS + a chat model that fit together, make the chat model Hot, enable Wyoming, give the HA connection details. Verify each by a test run.
2. "Find and run the latest Qwen MoE that fits": search, compare quants with fit estimates for this host at a target context, download, add with the right engine, load, test, report speed.
3. "Increase Ornith's context to 128k": read settings, preview memory fit, apply with reload, verify effective context, or explain the bound.
4. "Why was my last chat slow?": read request records (prompt processed vs cached, cache path, queue), engine/worker state, explain cause, suggest a fix (cache mode, slots, policy).
5. "Free up disk space": storage breakdown, unreferenced files, old engine builds, cache usage; delete only with reference checks and the permission level.
6. "Connect SillyTavern / Pi": endpoints, protocol status, model IDs, an API key created through the UI path (the secret is shown to the operator by the console, never placed in model context).
7. "Is the cache working?": session cache evidence, settings, real reuse from request records.
8. "How do I ...?": answer from bundled docs with links.

## Design rules

- Small discoverable index: the system prompt lists tool groups and one-line purposes only. `juiced_help(topic)` returns a shipped procedure (skill) for a job: steps, preconditions, the tools to use, how to verify, how to recover. Skills live in the repo as versioned markdown (e.g. service/src/barista/skills/*.md), included in the evaluation identity.
- One result envelope for every tool: `{ok, summary (one line for the model), data (bounded), source (route + revision/generation), changed (what was mutated, before -> after), job (id/state when async), next (suggested follow-up tools), warnings}`. Bounded output (truncate lists with counts and a cursor). No raw logs dumped: filters required.
- Mutations: carry the target's expected revision (the tool reads it if omitted, and the server still rejects stale writes); `preview: true` on every Configure/Maintain mutation returns the effect (memory fit, restart needed, files to delete, bytes freed) without changing anything. Permission levels: Full control = run; Ask before changes = the console shows a confirmation card with the preview and the operator approves; Read only = mutations refused with a clear message.
- Async work returns a job handle; `juiced_wait_job(id, max_seconds<=60)` waits server-side on the native job (no model polling loops). Long work continues after the chat turn ends; the console shows progress.
- Secrets never enter model context: key creation returns a handle; the console reveals the value to the operator.
- Every completion claim is backed by a verification tool result (load state, test output, effective settings).

## Tool catalog (target)

Inspect
- `juiced_status`: version, uptime, hardware summary, memory pools and headroom, loaded models, active/queued requests, alerts (Needs attention), pending jobs, cache and storage summary.
- `juiced_hardware`: devices (CPU, GPUs with vendor/arch/VRAM, NPU), drivers/runtime availability, which engine backends this host can run and why not.
- `juiced_list_models(filter)`: registered models with policy, state, engine, labels, per-request context, size on disk.
- `juiced_model_details(model)`: one model: files, engine build, saved/effective settings, capabilities (vision/thinking/tools), memory estimate at current settings, recent request stats, errors.
- `juiced_list_engines`: engine builds installed/installable/recommended per recipe with reasons and download sizes; which models use each.
- `juiced_requests(filter)`: recent request records (model, timing split, cached vs processed, cache path, outcome), bounded.
- `juiced_logs(filter: model|source, severity, since, limit<=200)`: bounded log lines.
- `juiced_storage`: disk usage by category (model weights, engines, session cache, downloads in progress, unreferenced files, legacy leftovers), free space per volume.
- `juiced_cache(model?)`: session cache settings, saved sessions per model, recent reuse evidence.
- `juiced_connections`: protocols (OpenAI, Ollama, Anthropic, A1111, MCP, Wyoming, Realtime) with enabled/health, URLs, model IDs, aliases, key status (counts only).

Acquire
- `juiced_search_models(query, task, max_size?)`: Hugging Face search grouped by repo with quants, sizes, license, vision/companion files, and a fit estimate for this host at a stated context.
- `juiced_inspect_repo(repo)`: files, quants, companions (mmproj), chat template capabilities, license, gated status.
- `juiced_fit(model or repo+quant, context, engine?)`: memory estimate and whether it fits alongside current Hot models; the same estimator the console uses.
- `juiced_download(repo, files|quant)`: job handle. `juiced_add_model(...)`: register with engine/build, policy, settings. `juiced_install_engine(recipe, backend)`: job handle.
- `juiced_wait_job(id, max_seconds)`, `juiced_cancel_job(id)`.

Configure
- `juiced_set_policy(model, policy)` (Disabled unloads), `juiced_load(model)`, `juiced_unload(model)`.
- `juiced_set_model_settings(model, changes, preview?, apply: next_load|reload)`: the full typed settings B6 adds (context, slots, sampling defaults, KV type, batch, flash-attn, extra args, image defaults).
- `juiced_set_cache(mode/budgets, model?)`, `juiced_set_protocol(protocol, enabled)`, `juiced_set_alias(alias, target)`, `juiced_set_wyoming(...)`.
- `juiced_create_api_key(name, scope)`: returns a handle; the console reveals the secret to the operator.

Verify and compare
- `juiced_test_model(model, kind: chat|embed|speech|transcribe|image|rerank, input?)`: one bounded real request through the normal API path; returns output excerpt, timings, tokens/s, cache path. Uses fixed shipped inputs by default.
- `juiced_benchmark(model, profile: quick)`: fixed shipped prompt set, cold/warm, reports measured numbers; restores prior state. (Full P2 comparison suite comes later; mark the quick profile as not a qualification.)

Maintain
- `juiced_remove_model(model, delete_files: bool, preview?)`: reference-checked; reports bytes freed.
- `juiced_delete_files(paths|ids, preview?)`: only unreferenced files the storage owner lists.
- `juiced_forget_sessions(model?|session)`, `juiced_uninstall_engine(recipe, backend, preview?)`.
- `juiced_pause_service(on|off)` (maintenance), `juiced_restart_model(model)`.

Explain
- `juiced_help(topic)`: shipped skills/procedures (setup-voice, find-and-run-model, increase-context, diagnose-slow, free-disk, connect-client, cache-check, engine-choice).
- `juiced_docs(page?, query?)`: bundled docs pages (B1's /v1/docs), with links to console locations.

Excluded by contract: shell, arbitrary filesystem, conversion/quantization, editing Barista's own permissions, cloud model substitution.

## Console

- Tool steps render as compact rows (helper-chat); a mutation row shows before -> after; job rows show live progress; confirmation cards for "Ask before changes" show the preview and Approve / Skip.
- Verification is visible: test results show inline (text excerpt, audio player, image thumbnail).

## Evaluation

- Controller tests for every tool: schema, authority (all three permission levels, revocation mid-turn, read key vs operator), stale revision, preview vs apply, envelope bounds, secret redaction.
- A scripted scenario suite for the eight jobs above against the fixture service (deterministic tool-call scripts), plus a real-model run of a subset with the largest cached small chat model locally, and on the homelab with Ornith after deploy (primary agent). The 60-task B1 qualification suite remains its own milestone; build the task file and runner so it can be executed, but do not claim qualification.
