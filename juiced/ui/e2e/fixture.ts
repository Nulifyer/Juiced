import { test as base, expect } from '@playwright/test';
export const modelId = 'Ornith-1.5-35B-A3B-GGUF-Q6_K';
export const otherId = 'Ornith-1.5-35B-A3B-GGUF-Q4_K_M';
export const test = base.extend<{ runtime: any }>({
  runtime: async ({ page }, use) => {
    const errors: string[] = [];
    page.on("pageerror", error => errors.push(error.message));
    const state = {
      stats: { tokens_per_second: 35, time_to_first_token: .25, input_tokens: 12, output_tokens: 32, cache_tokens: 2, request_count_total: 2, live: { active_requests: 0, generating_requests: 0, estimated_tokens_per_second: 0 } },
      preparedArtifacts: [{id: 'a'.repeat(64), bytes: 4096, quant: 'Q5_K_M', models: [modelId], removable: false}, {id: 'b'.repeat(64), bytes: 2048, quant: 'Q4_K_M', models: [], removable: true}],
      executionSelected: 'registered', preparationJobs: {} as Record<string, any>,
      saved: { ctx_size: 8192, llamacpp_args: '--parallel 2', llamacpp_backend: 'cpu' } as Record<string, any>,
      running: { ctx_size: 4096, llamacpp_args: '--parallel 2', llamacpp_backend: 'cpu' },
      defaults: { ctx_size: 4096, llamacpp_args: '', llamacpp_backend: 'cpu' },
      writes: [] as { path: string; body: any }[], failOptions: false, failRequests: false, empty: false, gpuInstalled: false,
      models: [
        { id: modelId, recipe: 'llamacpp', service: 'Chat', labels: ['tool-calling'], downloaded: true, available: true, loaded: true, state: 'loaded', policy: 'hot', size: 24, checkpoint: 'Example/Ornith-GGUF:Ornith-Q6_K.gguf', runtime: { pid: 4321, is_busy: false, settings_pending: true } },
        { id: otherId, recipe: 'llamacpp', service: 'Chat', labels: [], downloaded: true, available: true, loaded: false, state: 'unloaded', policy: 'disabled', size: 18, checkpoint: 'Example/Ornith-GGUF:Ornith-Q4_K_M.gguf' },
      ],
      cache: { settings: { mode: 'memory', retention_days: 7, disk_budget_gib: 8, model_modes: {} }, snapshots: [], persistent_supported: true },
      jobs: [ { id: otherId, model_name: 'user.' + otherId, type: 'model', complete: true, status: 'complete', percent: 100 }, { id: 'backend:whispercpp:cpu', model_name: 'whispercpp:cpu', type: 'backend', complete: true, status: 'complete' } ],
      requests: [ { id: 2, timestamp_ms: Date.now(), model: modelId, endpoint: '/v1/chat/completions', protocol: 'openai', stream: false, status: 200, duration_ms: 1532, input_tokens: 12, output_tokens: 32 }, { id: 1, timestamp_ms: Date.now(), model: null, endpoint: '/api/chat', protocol: 'ollama', stream: true, status: 400, duration_ms: 5, input_tokens: null, output_tokens: null } ],
    };
    await page.routeWebSocket('**/logs/stream', socket => { socket.close({ code: 1000, reason: 'Fixture has no log stream' }); });
    await page.route('https://huggingface.co/**', route => route.fulfill({ body: '# Ornith model card\n\nA public model card.\n\n<script>window.compromised=true</script>', contentType: 'text/markdown' }));
    await page.route('**/v1/**', async route => {
      const request = route.request(), path = new URL(request.url()).pathname.slice(4);
      const body = request.method() === 'POST' ? request.postDataJSON() : undefined;
      const reply = (json: any, status = 200) => route.fulfill({ json, status });
      if (body) state.writes.push({ path, body });
      if (/^models\/.+\/options$/.test(path)) {
        if (state.failOptions) return reply({ error: 'Fixture settings unavailable' }, 503);
        if (body) Object.assign(state.saved, body);
        return reply({ recipe: 'llamacpp', saved: state.saved, defaults: state.defaults, effective: { ...state.defaults, ...state.saved } });
      }
      if (path === 'juiced/state') return reply({ version: 'v0.9.2', catalog_ready: true });
      if (path === 'juiced/catalog') return reply({ data: state.empty ? [] : state.models });
      if (path === 'juiced/transfers' || path === 'downloads') return reply({ data: state.jobs });
      if (path === 'juiced/cache') { if (body) state.cache.settings = { ...state.cache.settings, ...body }; return reply(state.cache); }
      if (path === 'juiced/protocols') return reply({ protocols: [
        { id: 'openai', base_path: '/v1', features: ['Chat', 'Speech', 'Images'], limits: ['Model and backend capabilities apply.'] },
        { id: 'ollama', base_path: '', features: ['Chat', 'Generate', 'Tags'], limits: ['Model installation uses the Juiced catalog.'] },
        { id: 'wyoming', features: ['STT', 'TTS'], limits: ['Private TCP listener.'], listener: { enabled: true, process_running: true, uri: 'tcp://0.0.0.0:10300' } },
      ] });
      if (path === 'health') return reply({ all_models_loaded: [{ model_name: modelId, recipe_options: state.running, context_observation: { source: 'llamacpp:/props', per_request_tokens: 2048, slots: 2 }, launch_command: ['/runtime/llama-server', '--model', '/library/Ornith-Q6_K.gguf', '--ctx-size', '4096', '--parallel', '2'] }] });
      if (path === 'juiced/operation') return reply({ status: 'queued' });
      if (path === 'juiced/policy') { const model = state.models.find(m => m.id === body.model); if (model) model.policy = body.policy; return reply({}); }
      if (path === 'registry/search') return reply({ results: [{ repository_id: 'Example/Ornith-GGUF', downloads: 5000 }] });
      if (path === 'pull/variants') return reply({ checkpoint: 'Example/Ornith-GGUF', recipe: 'llamacpp', suggested_name: 'Ornith-GGUF', variants: [{ name: 'Ornith-Q6_K.gguf', size_bytes: 24000000000 }, { name: 'Ornith-Q4_K_M.gguf', size_bytes: 18000000000 }], mmproj_files: [] });
      if (path === 'juiced/execution') {
        if (body) { state.executionSelected = body.profile; return reply({ accepted: true }, 202); }
        return reply({ model: modelId, selected: state.executionSelected, architecture: 'qwen35moe', activity: state.executionSelected === 'llamacpp:hybrid-rocm' ? { npu_matmul_nodes: 48, prefill_plans: 2, npu_planned_weight_bytes: 104857600, npu_copy_budget_bytes: 2147483648 } : undefined, profiles: [
          { id: 'registered', recipe: 'llamacpp', compatible: true, available: true, changes: [] },
          { id: 'llamacpp:cpu', recipe: 'llamacpp', backend: 'cpu', compatible: true, available: true, package_state: 'installed', reason: 'Reuse the downloaded GGUF without changing its weights', changes: [] },
          { id: 'llamacpp:vulkan', recipe: 'llamacpp', backend: 'vulkan', compatible: true, available: true, package_state: state.gpuInstalled ? 'installed' : 'installable', reason: 'Reuse the downloaded GGUF without changing its weights', changes: [] },
          { id: 'llamacpp:rocm', recipe: 'llamacpp', backend: 'rocm', compatible: true, available: false, hardware_reason: 'Requires an available ROCm GPU', changes: [] },
          { id: 'llamacpp:hybrid-rocm', recipe: 'llamacpp', backend: 'hybrid-rocm', compatible: true, available: true, experimental: true, package_state: 'installable', changes: ['Eligible NPU prefill uses reduced-precision arithmetic; GPU weights and decode retain the source quantization', 'Partial prefill only: routed experts, attention and decode stay on ROCm'] },
          { id: 'vllm:rocm', recipe: 'vllm', backend: 'rocm', compatible: false, available: true, reason: 'The pinned vLLM GGUF importer does not support this architecture', changes: [] },
        ] });
      }
      if (path === 'juiced/preparation') {
        if (body) {
          if (body.action === 'delete_cache') { state.preparedArtifacts = state.preparedArtifacts.filter(a => a.id !== body.id); return reply({removed: true}); }
          if (body.action === 'cancel') { state.preparationJobs[body.id].pending = false; state.preparationJobs[body.id].phase = 'cancelled'; return reply({ accepted: true }); }
          return reply({error:{code:'feature_retired',message:'Find an existing published variant in Library'}},410);
        }
        if (new URL(request.url()).searchParams.has('cache')) return reply({artifacts: state.preparedArtifacts});
        if (new URL(request.url()).searchParams.has('model')) return reply({error:{code:'feature_retired'}},410);
        return reply({ jobs: state.preparationJobs });
      }
      if (path === 'juiced/devices') return reply({ devices: [{ id: 'Vulkan0', name: 'AMD Radeon 890M Graphics' }] });
      if (path === 'system-info') return reply({ devices: { cpu: { name: 'Test CPU', cores: 8, threads: 16, available: true }, gpu: [{ name: 'Test GPU', available: true }] }, recipes: { llamacpp: { display_name: 'llama.cpp', modality: 'llm', backends: { cpu: { state: 'update_available', version: 'b123', message: 'Update is available' }, vulkan: { state: state.gpuInstalled ? 'installed' : 'installable' }, rocm: { state: 'unsupported' } } }, whispercpp: { display_name: 'Whisper.cpp', modality: 'stt', backends: { cpu: { state: 'installed', version: 'v1.8.0' } } } } });
      if (path === 'system-stats') return reply({ cpu_percent: 25, memory_gb: 34, npu_percent: 0, primary_gpu_id: 'gpu0', gpus: [{ id: 'gpu0', name: 'Test GPU', gpu_percent: 40, memory_used_gb: 24, memory_type: 'shared' }] });
      if (path === 'stats') return reply(state.stats);
      if (path === 'juiced/requests') return state.failRequests ? reply({ error: 'Fixture diagnostics unavailable' }, 503) : reply({ data: state.requests });
      if (path === 'juiced/removal' || path === 'juiced/removal' && !body) return reply({ allowed: false, description: 'External weights are preserved', reason: 'Disable and unload this model before removal', label: 'Remove model' });
      return reply({});
    });
    await use(state);
    expect(errors, "No unhandled browser exceptions").toEqual([]);
  },
});
export { expect };
export async function openModel(page: any, id = modelId) {
  await page.goto('/');
  if (id !== modelId) await page.getByRole('button', { name: /Show 1 model/ }).click();
  await page.getByRole('button', { name: id, exact: true }).first().click();
  await expect(page.getByRole('region', { name: 'Model details', exact: true })).toBeVisible();
}
