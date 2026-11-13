import { test as base, expect, type APIRequestContext, type Locator, type Page } from "@playwright/test";

export { expect };
export const sourceOrigin = `http://127.0.0.1:${process.env.JUICED_SOURCE_PORT ?? 13308}`;
export const hotModel = "fixture.chat-small-q4";
export const modelId = "fixture.chat-long-context-q8";
export const speechModel = "fixture.speech-tiny";
export type Policy = "hot" | "on-demand" | "disabled";
export type CatalogModel = { id: string; policy: Policy; revision: number; loaded: boolean; state: string; error: string | null; pid: number | null };
export type SourceFixture = { model_file: string; upload_file: string; import_root: string; source_url: string; sha256: string; bytes: number; command: string; args: string[] };

export function api(request: APIRequestContext) {
  async function json<T>(path: string): Promise<T> {
    const response = await request.get(path);
    expect(response.ok(), `${path}: ${await response.text()}`).toBe(true);
    return response.json();
  }
  async function post<T = unknown>(path: string, data: unknown, ok = true): Promise<T> {
    const response = await request.post(path, { data });
    if (ok) expect(response.ok(), `${path}: ${await response.text()}`).toBe(true);
    return response.json();
  }
  const catalog = async () => (await json<{ data: CatalogModel[] }>("/v1/juiced/catalog")).data;
  const model = async (id: string) => (await catalog()).find((entry) => entry.id === id);
  async function policy(id: string, value: Policy) {
    const current = await model(id);
    expect(current, id).toBeTruthy();
    if (current!.policy === value) return current!;
    return post<CatalogModel>("/v1/juiced/policy", { model: id, policy: value, expected_revision: current!.revision });
  }
  async function unload(id: string) {
    const current = await model(id);
    if (current?.loaded || current?.state === "cleanup-pending") await post("/v1/unload", { model: id });
  }
  async function remove(id: string) {
    const current = await model(id);
    if (!current) return;
    if (current.policy === "hot") await policy(id, "on-demand");
    await unload(id);
    const disabled = await policy(id, "disabled");
    await post("/v1/delete", { model: id, expected_revision: disabled.revision });
  }
  async function register(spec: Record<string, unknown>) {
    return post<CatalogModel>("/v1/models/register", { resident_bytes: 1048576, max_inflight: 1, operations: ["chat/completions", "completions"], policy: "disabled", revision: 0, ...spec });
  }
  const source = () => json<SourceFixture>(`${sourceOrigin}/fixture.json`);
  const artifacts = (query = "") => json<any>(`/v1/juiced/artifacts${query}`);
  async function removeArtifact(sha: string) {
    const inventory = await artifacts("?limit=128");
    for (const job of inventory.jobs.filter((job: any) => job.expected_sha256 === sha || job.sha256 === sha || job.artifact === sha)) {
      await request.post(`/v1/juiced/artifacts/jobs/${job.id}`, { data: { action: "forget", expected_revision: job.revision } });
    }
    const current = await artifacts("?limit=128");
    if (current.artifacts.some((item: any) => item.id === sha)) await post("/v1/juiced/artifacts/remove", { artifact: sha, expected_generation: current.generation });
  }
  async function forgetCompleteJobs() {
    const inventory = await artifacts("?limit=128");
    for (const job of inventory.jobs.filter((job: any) => !job.writer_active && ["complete", "cancelled"].includes(job.state))) {
      await request.post(`/v1/juiced/artifacts/jobs/${job.id}`, { data: { action: "forget", expected_revision: job.revision } });
    }
  }
  async function createSet(sha: string, bytes: number, importer = "browser-journey") {
    const inventory = await artifacts();
    return post<{ id: string }>("/v1/juiced/artifacts/sets", { expected_generation: inventory.generation, specification: { schema_version: 1, origin: { importer_id: importer, importer_version: "1", source_revision: crypto.randomUUID() }, primary: "model", members: [{ name: "model", role: "model", artifact: sha, bytes }] } });
  }
  async function removeSet(set: string) {
    const inventory = await artifacts();
    await post("/v1/juiced/artifacts/sets/remove", { set, expected_generation: inventory.generation });
  }
  async function paused(value: boolean) {
    const admission = await json<{ paused: boolean }>("/v1/juiced/admission");
    if (admission.paused !== value) await post("/v1/juiced/admission", { paused: value });
  }
  async function acquire(fixture: SourceFixture) {
    const inventory = await artifacts();
    if (inventory.artifacts.some((item: any) => item.id === fixture.sha256)) return;
    await post("/v1/juiced/artifacts/pull", { download: { source_url: fixture.source_url, expected_sha256: fixture.sha256, expected_bytes: fixture.bytes, maximum_bytes: fixture.bytes }, expected_generation: inventory.generation });
    await expect.poll(async () => (await artifacts()).artifacts.some((item: any) => item.id === fixture.sha256), { timeout: 15000 }).toBe(true);
    await forgetCompleteJobs();
  }
  const barista = () => json<{ enabled: boolean; model: string | null; permission: "full" | "ask" | "read_only"; revision: number; tools: { name: string }[] }>("/v1/juiced/barista");
  async function setBarista(enabled: boolean, model: string | null = hotModel, permission: "full" | "ask" | "read_only" = "full") {
    const current = await barista();
    if (current.enabled === enabled && (!enabled || (current.model === model && current.permission === permission))) return current;
    return post<Awaited<ReturnType<typeof barista>>>("/v1/juiced/barista", { expected_revision: current.revision, enabled, model: model ?? current.model, permission });
  }
  return { acquire, json, post, catalog, model, policy, unload, remove, register, source, artifacts, removeArtifact, forgetCompleteJobs, createSet, removeSet, paused, barista, setBarista };
}

export const test = base.extend<{ baseline: void }>({
  baseline: [async ({ request }, use) => {
    const service = api(request);
    await service.paused(false);
    await service.setBarista(false);
    await service.policy(modelId, "on-demand");
    await service.unload(modelId);
    await service.policy(speechModel, "on-demand");
    await service.policy(hotModel, "hot");
    await expect.poll(async () => (await service.model(hotModel))?.loaded, { timeout: 15000 }).toBe(true);
    await use();
  }, { auto: true }],
});

export async function open(page: Page) {
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Dashboard", level: 1 })).toBeVisible();
  await expect(page.getByRole("region", { name: "Running models" })).toBeVisible();
}

export async function navigate(page: Page, name: string, arrive = true) {
  const menu = page.getByRole("button", { name: "Toggle navigation" });
  if (await menu.isVisible() && await menu.getAttribute("aria-expanded") !== "true") {
    await menu.click();
    await expect(menu).toHaveAttribute("aria-expanded", "true");
  }
  await page.getByRole("navigation", { name: "Destinations" }).getByRole("button", { name: name === "Models" ? /^Models(?: \d+)?$/ : name, exact: name !== "Models" }).click();
  if (arrive) await expect(page.getByRole("heading", { name, level: 1, exact: true })).toBeVisible();
}

export async function serviceMenu(page: Page, item: string) {
  await page.getByRole("button", { name: "Service status" }).click();
  await page.getByRole("menu").getByRole("menuitem", { name: item }).click();
}

export const toasts = (page: Page) => page.locator(".toasts");
export const tab = (page: Page, list: string, name: string) => page.getByRole("tablist", { name: list }).getByRole("tab", { name, exact: true });

export type ModelsView = "Installed" | "Discover" | "Downloads" | "Aliases";
export const modelsTab = (page: Page, view: ModelsView) => page.getByRole("tablist", { name: "Models views" }).getByRole("tab", view === "Downloads" ? { name: /^Downloads(?: \d+)?$/ } : { name: view, exact: true });

export async function modelsView(page: Page, view: ModelsView) {
  await navigate(page, "Models");
  await modelsTab(page, view).click();
  await expect(modelsTab(page, view)).toHaveAttribute("aria-selected", "true");
}

export async function openModel(page: Page, id: string) {
  await navigate(page, "Models");
  await page.getByRole("table", { name: "Registered models" }).getByRole("button", { name: id, exact: true }).click();
  const inspector = page.getByRole("complementary", { name: "Model inspector" });
  await expect(inspector.getByRole("heading", { name: id, level: 2 })).toBeVisible();
  return inspector;
}

export async function closeInspector(page: Page) {
  const back = page.getByRole("button", { name: "Back to models", exact: true });
  if (await back.isVisible()) await back.click();
  else await page.getByRole("button", { name: "Close model inspector", exact: true }).click();
}

export async function noHorizontalOverflow(page: Page, label: string) {
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), `${label}: page fits the viewport`).toBe(true);
  const offenders = await page.locator("main").evaluate((main) => {
    if (main.scrollWidth <= main.clientWidth + 1) return [];
    const limit = main.getBoundingClientRect().right + 1;
    return [...main.querySelectorAll("*")].filter((element) => element.getBoundingClientRect().right > limit && !(element.parentElement && element.parentElement.getBoundingClientRect().right > limit))
      .slice(0, 5).map((element) => `${element.tagName.toLowerCase()}.${element.className} "${(element.textContent ?? "").slice(0, 40)}"`);
  });
  expect(offenders, `${label}: workspace fits its scroll container`).toEqual([]);
}

export async function largeText(page: Page) {
  await page.evaluate(() => { document.documentElement.style.fontSize = "200%"; });
}

export async function storedText(page: Page) {
  return page.evaluate(() => { try { return Object.entries(localStorage).map(([key, value]) => `${key}=${value}`).join("\n"); } catch { return ""; } });
}

export function rowFor(table: Locator, id: string) {
  return table.getByRole("row").filter({ has: table.page().getByRole("button", { name: id, exact: true }) });
}

export async function tryModel(page: Page, id: string) {
  await page.getByRole("region", { name: "Try a model" }).getByLabel("Model").selectOption(id);
}

/** Reports the model as accepting images, as a llama.cpp worker with a vision projector does. */
export async function withVision(page: Page, id: string) {
  await page.route("**/v1/juiced/catalog", async (route) => {
    const response = await route.fetch();
    const body = await response.json();
    for (const model of body.data) if (model.id === id) model.vision = true;
    await route.fulfill({ response, json: body });
  });
}

export async function picture(page: Page) {
  return Buffer.from(await page.evaluate(() => {
    const canvas = document.createElement("canvas"); canvas.width = 320; canvas.height = 200;
    const context = canvas.getContext("2d")!;
    const gradient = context.createLinearGradient(0, 0, 320, 200); gradient.addColorStop(0, "#8cacf1"); gradient.addColorStop(1, "#e5bd7b");
    context.fillStyle = gradient; context.fillRect(0, 0, 320, 200);
    context.fillStyle = "#0d0e10"; context.font = "bold 40px sans-serif"; context.fillText("GPU 72%", 70, 115);
    return canvas.toDataURL("image/png").split(",")[1]!;
  }), "base64");
}

/** Screenshots at the four review sizes when JUICED_BARISTA_SHOTS names a directory. */
export async function capture(page: Page, name: string, ready: () => Promise<void>) {
  const directory = process.env.JUICED_BARISTA_SHOTS;
  if (!directory) return;
  const original = page.viewportSize()!;
  for (const [width, height] of [[390, 844], [820, 1180], [1280, 900], [2359, 1409]] as const) {
    await page.setViewportSize({ width, height });
    await ready();
    await page.screenshot({ path: `${directory}/${name}-${width}x${height}.png` });
  }
  await page.setViewportSize(original);
}
