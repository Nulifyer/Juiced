---
topic: compare-variants
version: 1
title: Compare two variants
summary: Resolve both exact variants, run the fixed quick benchmark on each with the same settings, restore prior state and report measured differences.
---
# Compare two variants

1. `juiced_model_details` on both: same context and slots, or note the difference.
2. `juiced_fit` on both, and `juiced_status` for headroom. Only one may fit at a time; then they run one after the other.
3. `juiced_benchmark` with `profile: quick` on each. It uses a fixed shipped prompt set, measures cold and warm runs and restores the model's prior load state.
4. Report the measured numbers side by side: load time, prompt and decode tokens per second. Note that the quick profile is not a quality or qualification result; it measures speed only.
