import { api, expect, modelId, navigate, open, test, toasts } from "./support";

type Residency = { configuration_generation: number; idle_unload_after_ms: number; models: { id: string; residency: { model_idle_unload_after_ms: number | null } }[] };

test("idle unload is set for the service and for one On demand model", async ({ page, request }) => {
  const service = api(request);
  const before = await service.json<Residency>("/v1/juiced/residency");
  try {
    await open(page);
    await navigate(page, "Loading policy");
    const lane = page.getByRole("region", { name: "On demand models" });
    const global = lane.getByLabel("Unload idle On demand models after");
    await expect(global).toHaveValue(String(before.idle_unload_after_ms));
    await global.selectOption({ label: "after 5 min" });
    await expect(toasts(page).getByRole("status")).toContainText("Idle On demand models unload after 5 min.");
    await expect.poll(async () => (await service.json<Residency>("/v1/juiced/residency")).idle_unload_after_ms).toBe(300000);

    await lane.getByRole("button", { name: new RegExp(modelId) }).click();
    const inspector = page.getByRole("complementary", { name: "Model inspector" });
    const own = inspector.getByLabel(`Idle unload for ${modelId}`);
    await expect(own).toHaveValue("inherit");
    await expect(own.locator("option").first()).toHaveText("Default (after 5 min)");
    await own.selectOption({ label: "never" });
    await expect(toasts(page).getByRole("status")).toContainText(`${modelId} stays loaded until memory is needed.`);
    await expect.poll(async () => (await service.json<Residency>("/v1/juiced/residency")).models.find((item) => item.id === modelId)?.residency.model_idle_unload_after_ms).toBe(0);
    await own.selectOption("inherit");
    await expect(toasts(page).getByRole("status")).toContainText(`${modelId} follows the service default.`);
  } finally {
    const now = await service.json<Residency>("/v1/juiced/residency");
    if (now.models.find((item) => item.id === modelId)?.residency.model_idle_unload_after_ms !== null) await service.post("/v1/juiced/residency", { expected_generation: now.configuration_generation, model: modelId, idle_unload_after_ms: null });
    const latest = await service.json<Residency>("/v1/juiced/residency");
    if (latest.idle_unload_after_ms !== before.idle_unload_after_ms) await service.post("/v1/juiced/residency", { expected_generation: latest.configuration_generation, idle_unload_after_ms: before.idle_unload_after_ms });
  }
});
