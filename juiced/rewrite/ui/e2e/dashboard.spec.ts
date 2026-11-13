import { api, expect, hotModel, navigate, open, tab, test } from "./support";

test("glance tiles and the running model render from service data", async ({ page }) => {
  await open(page);
  const glance = page.getByRole("region", { name: "At a glance" });
  await expect(glance.getByRole("article", { name: "Protocols" })).toContainText(/\d+ ready/);
  await expect(glance.getByRole("article", { name: "Requests" })).toContainText(/\d+ active/);
  await expect(glance.getByRole("article", { name: "Requests" })).toContainText(/of \d+ slots?/);
  await expect(glance.getByRole("article", { name: "Model memory" })).toContainText("/ 6.0 GiB");
  await expect(glance.getByRole("article", { name: "Model memory" })).toContainText("free for another model");
  await expect(glance.getByRole("meter", { name: "Model memory reserved" })).toBeVisible();
  await expect(page.getByRole("img", { name: /over the last two minutes/ }), "resource history lives in Diagnostics").toHaveCount(0);
  const running = page.getByRole("region", { name: "Running models" });
  await expect(running.getByRole("button", { name: new RegExp(hotModel) })).toContainText("ready · Hot");
  await expect(running.getByRole("button", { name: new RegExp(hotModel) })).toContainText(/Used .+ago|Used just now|Not used yet/);
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

test("a failed request needs attention and opens Diagnostics requests", async ({ page, request }) => {
  const failure = await request.post("/v1/chat/completions", { data: { model: hotModel, messages: [{ role: "user", content: "Dashboard failure prompt" }], stream: false, max_tokens: 4, fixture_status: 500 } });
  expect(failure.ok()).toBe(false);
  await open(page);
  const attention = page.getByRole("region", { name: "Needs attention" });
  const entry = attention.getByRole("button", { name: /failed requests?/ });
  await expect(entry).toContainText(`Latest: ${hotModel}`);
  await expect(page.locator("body")).not.toContainText("Dashboard failure prompt");
  await entry.click();
  await expect(page.getByRole("heading", { name: "Diagnostics", level: 1 })).toBeVisible();
  await expect(tab(page, "Diagnostics views", "Requests")).toHaveAttribute("aria-selected", "true");
  await expect(page.getByRole("table", { name: "Recent requests" }).getByRole("row").filter({ hasText: hotModel }).filter({ hasText: "500" }).first()).toBeVisible();
});

test("the GPU memory tile names the GPU and says when utilization has no reading", async ({ page }) => {
  let gpu: Record<string, unknown> | null = { id: "amdgpu:0000:c7:00.0", vendor: "amd", name: null, gpu_percent: 0, memory_used_bytes: 5 * 1024 ** 3, memory_total_bytes: 16 * 1024 ** 3 };
  await page.route("**/v1/system-stats", async (route) => {
    const stats = await (await route.fetch()).json();
    await route.fulfill({ json: { ...stats, gpus: gpu ? [gpu] : [] } });
  });
  await open(page);
  const card = page.getByRole("region", { name: "At a glance" }).getByRole("article", { name: "GPU memory" });
  await expect(card).toContainText("5.0 / 16 GiB");
  await expect(card).toContainText("0.0% busy · AMD GPU");
  await expect(card.getByRole("meter", { name: "GPU memory used" })).toBeVisible();
  gpu = { ...gpu, name: "AMD Radeon 890M", gpu_percent: null };
  await expect(card).toContainText("Utilization unavailable · AMD Radeon 890M");
  await expect(card).not.toContainText("No GPU detected");
  gpu = null;
  await expect(card).toContainText("No GPU detected");
  await expect(card.getByRole("meter")).toHaveCount(0);
});
