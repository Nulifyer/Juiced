---
topic: diagnose-slow
version: 1
title: Find why a request was slow
summary: Read request records for prompt processed versus cached, queue wait and decode speed, check worker state and suggest one concrete fix.
---
# Find why a request was slow

1. `juiced_requests` filtered to the model (and `limit` 10). For each slow record compare:
   - `queue_wait_ms`: time waiting for a free slot. High means too few `parallel_slots` or another long request.
   - `prompt_processed_tokens` versus `input_tokens`: tokens prefilled again. A large gap that is not cached means the prompt cache missed.
   - `prompt_tokens_per_second` and `decode_tokens_per_second`: engine speed.
   - `session_cache.path`: how the session cache served the request (`restored`, `resident`, `miss` and so on).
2. `juiced_model_details`: was the worker just loaded (cold start), is it partly on CPU (`gpu_layers`), what context and slots it uses.
3. `juiced_status`: other active requests, memory pressure, paused admission.
4. `juiced_cache` for the model: is the session cache on for it and is it effective.
5. Name the cause from the evidence, then one fix: enable the session cache, add a slot, keep the model Hot, offload more layers, or a smaller context. Offer to apply it; preview first.

Never claim a cache hit unless a request record shows cached or restored tokens.
