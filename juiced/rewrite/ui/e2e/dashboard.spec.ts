import { api, expect, hotModel, navigate, open, test } from "./support";

test("live resource cards and the running model render from service data", async ({ page }) => {
  await open(page);
  const resources = page.getByRole("region", { name: "Live resources" });
  await expect(resources.getByRole("article", { name: "CPU" })).toContainText(/\d+(\.\d)?%/);
  await expect(resources.getByRole("article", { name: "Memory" })).toContainText(/\d+(\.\d)? GiB/);
  await expect(resources.getByRole("article", { name: "Memory" })).toContainText(/of \d+(\.\d)? GiB/);
  await expect(resources.getByRole("article", { name: "GPU" })).toBeVisible();
  await expect(resources.getByRole("article", { name: "VRAM" })).toBeVisible();
  await expect(resources.getByRole("img", { name: "CPU over the last two minutes" })).toBeVisible();
  const running = page.getByRole("region", { name: "Running models" });
  await expect(running.getByRole("button", { name: new RegExp(hotModel) })).toContainText("ready · Hot");
  await expect(running).toContainText(/Hot \d+\/\d+ ready/);
  await expect(page.getByRole("button", { name: /refresh/i })).toHaveCount(0);
  await running.getByRole("button", { name: new RegExp(hotModel) }).click();
  await expect(page.getByRole("heading", { name: "Models", level: 1 })).toBeVisible();
  await expect(page.getByRole("complementary", { name: "Model inspector" }).getByRole("heading", { name: hotModel })).toBeVisible();
});

test("a failed Hot model needs attention and opens in the Models inspector", async ({ page, request }) => {
  const service = api(request);
  const fixture = await service.source();
  const id = `browser.failing-${test.info().project.name}`;
  await service.remove(id);
  try {
    await service.register({ id, model_file: fixture.model_file, command: fixture.command, args: [...fixture.args, "--mode", "exit"] });
    await service.policy(id, "on-demand");
    expect((await request.post("/v1/load", { data: { model: id } })).ok()).toBe(false);
    await service.policy(id, "hot");
    await open(page);
    const attention = page.getByRole("region", { name: "Needs attention" });
    const entry = attention.getByRole("button", { name: new RegExp(id.replaceAll(".", "\\.")) });
    await expect(entry).toContainText("Failed to start");
    await expect(page.getByRole("region", { name: "Running models" })).not.toContainText(id);
    await entry.click();
    const inspector = page.getByRole("complementary", { name: "Model inspector" });
    await expect(inspector.getByRole("heading", { name: id })).toBeVisible();
    await expect(inspector.getByRole("alert")).toBeVisible();
    await expect(page.getByRole("heading", { name: "Models", level: 1 })).toBeVisible();
  } finally {
    await service.remove(id);
  }
  await navigate(page, "Dashboard");
  await expect(page.getByRole("region", { name: "Needs attention" })).not.toContainText(id);
});

test("recent requests list completed work and link to the test page", async ({ page, request }) => {
  const response = await request.post("/v1/chat/completions", { data: { model: hotModel, messages: [{ role: "user", content: "Dashboard fixture prompt" }], stream: false, max_tokens: 4 } });
  expect(response.ok()).toBe(true);
  await open(page);
  const recent = page.getByRole("region", { name: "Recent requests" });
  await expect(recent).toContainText(hotModel);
  await expect(recent).toContainText("chat/completions");
  await expect(recent).not.toContainText("Dashboard fixture prompt");
  await recent.getByRole("button", { name: "Try a model" }).click();
  await expect(page.getByRole("heading", { name: "Test services", level: 1 })).toBeVisible();
});

test("the GPU card names the GPU and says when utilization has no reading", async ({ page }) => {
  let gpu: Record<string, unknown> | null = { id: "amdgpu:0000:c7:00.0", vendor: "amd", name: null, gpu_percent: 0, memory_used_bytes: 5 * 1024 ** 3, memory_total_bytes: 16 * 1024 ** 3 };
  await page.route("**/v1/system-stats", async (route) => {
    const stats = await (await route.fetch()).json();
    await route.fulfill({ json: { ...stats, gpus: gpu ? [gpu] : [] } });
  });
  await open(page);
  const card = page.getByRole("region", { name: "Live resources" }).getByRole("article", { name: "GPU" });
  await expect(card).toContainText("0.0%");
  await expect(card).toContainText("AMD GPU");
  await expect(page.getByRole("article", { name: "VRAM" })).toContainText("of 16 GiB");
  gpu = { ...gpu, name: "AMD Radeon 890M", gpu_percent: null };
  await expect(card).toContainText("Utilization unavailable");
  await expect(card).not.toContainText("No GPU detected");
  gpu = null;
  await expect(card).toContainText("No GPU detected");
});
