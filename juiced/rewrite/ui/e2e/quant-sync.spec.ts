import { api, expect, noHorizontalOverflow, open, openModel, tab, test } from "./support";

test("copies launch settings to other quants and restarts the loaded one", async ({ page, request }) => {
  const service = api(request);
  const fixture = await service.source();
  const base = `Quant${test.info().project.name}-7B-GGUF`;
  const [source, q6, q8] = ["Q4_K_M", "Q6_K", "Q8_0"].map((quant) => `${base}-${quant}`) as [string, string, string];
  const settings = { context_tokens: 2048, threads: 3, gpu_layers: 0, parallel_slots: 1 };
  const register = (id: string, options: Record<string, unknown>) => service.register({ id, model_file: fixture.model_file, provider: "llama-cpp", options, command: fixture.command, args: [...fixture.args, "--parallel", "1", "--ctx-size", "1024"] });
  for (const id of [source, q6, q8]) await service.remove(id);
  await register(source, settings);
  await register(q6, {});
  await register(q8, { threads: 1 });
  try {
    await service.policy(q8, "on-demand");
    await service.post("/v1/load", { model: q8 });
    const pid = (await service.model(q8))!.pid;
    await open(page);
    const inspector = await openModel(page, source);
    await tab(page, "Model sections", "Settings").click();
    const panel = inspector.getByRole("region", { name: "Other quants" });
    const six = panel.getByRole("checkbox", { name: /^Q6_K/ });
    const eight = panel.getByRole("checkbox", { name: /^Q8_0/ });
    await expect(six).toBeEnabled();
    await expect(panel).toContainText("Differs: context, threads, GPU layers, slots");
    await expect(panel).toContainText("restarts");
    const copy = panel.getByRole("button", { name: "Copy settings" });
    await expect(copy).toBeDisabled();
    await six.check();
    await eight.check();
    await noHorizontalOverflow(page, "quant sync");
    await panel.getByRole("button", { name: "Copy to 2 quants" }).click();
    const report = panel.getByRole("list", { name: "Copy results" });
    await expect(report).toContainText("Q6_K Copied context, threads, GPU layers, slots.");
    await expect(report).toContainText("Q8_0 Copied context, threads, GPU layers, slots. Restarted.");
    await expect(six).toBeDisabled();
    await expect(panel).not.toContainText("Differs");
    for (const id of [q6, q8]) expect((await service.json<{ saved: unknown }>(`/v1/models/${id}/options`)).saved).toEqual(settings);
    const reloaded = (await service.model(q8))!;
    expect(reloaded.loaded).toBe(true);
    expect(reloaded.pid).not.toBe(pid);
    await noHorizontalOverflow(page, "quant sync report");
  } finally {
    for (const id of [source, q6, q8]) await service.remove(id);
  }
});

test("other quants stay hidden for a model without quant siblings", async ({ page }) => {
  await open(page);
  const inspector = await openModel(page, "fixture.chat-long-context-q8");
  await tab(page, "Model sections", "Settings").click();
  await expect(inspector.getByRole("region", { name: "Context and launch profile" })).toBeVisible();
  await expect(inspector.getByRole("region", { name: "Other quants" })).toHaveCount(0);
});
