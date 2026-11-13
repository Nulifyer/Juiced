---
topic: engine-choice
version: 1
title: Choose an engine and backend
summary: Match a model to an engine build this host can run, install it, and explain backends the hardware cannot use.
---
# Choose an engine and backend

1. `juiced_hardware`: devices, drivers and which backends are `available` here, with the reason for each one that is not.
2. `juiced_list_engines`: installed builds, installable builds with download size, the recommended backend per engine, and which models use each build.
3. Pick the recommended backend unless the operator asked for another. Typical order: CUDA or ROCm on a supported GPU, then Vulkan, then CPU.
4. `juiced_install_engine` with `preview: true` to see the download, then apply and `juiced_wait_job`.
5. A model chooses its engine when added (`juiced_add_model` with `backend`). To change an existing model's build, explain it is done from the model's Engine settings in the console.

A backend this hardware cannot run is not an error to fix: say it is unavailable here and why.
