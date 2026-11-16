import type { APIRequestContext, Page } from "@playwright/test";
import { api, closeInspector, expect, modelsView, navigate, open, test, toasts, tryModel } from "./support";

const repo = "fixture-labs/Tiny-Chat-GGUF";
const file = "Tiny-Chat-Q8_0.gguf";
const added = "Tiny-Chat-Q8_0";
const result = (page: Page) => page.getByRole("list", { name: "Search results" }).getByRole("listitem").filter({ has: page.getByText(/^Tiny-Chat-GGUF$/) });

function library(request: APIRequestContext) {
  const service = api(request);
  async function sha() {
    const search = await service.json<any>("/v1/juiced/library/search?q=tiny-chat&limit=12");
    return search.data.find((model: any) => model.repo === repo).quants.find((quant: any) => quant.file === file).sha256 as string;
  }
  async function engines() { return service.json<any>("/v1/juiced/engines"); }
  const cpuEngine = (document: any) => document.installed.find((item: any) => item.engine === "llama.cpp" && item.release?.backend === "cpu");
  async function installEngine() {
    const document = await engines();
    if (cpuEngine(document)) return;
    const started = await service.post<any>("/v1/juiced/engines/install", { engine: "llama.cpp", backend: "cpu", expected_generation: document.generation });
    if (started.already_installed) return;
    await expect.poll(async () => (await engines()).jobs.find((job: any) => job.id === started.job.id)?.state, { timeout: 30000 }).toBe("complete");
  }
  async function pull(artifact: string) {
    const search = await service.json<any>("/v1/juiced/library/search?q=tiny-chat&limit=12");
    if ((await service.artifacts()).artifacts.some((item: any) => item.id === artifact)) return;
    await service.post("/v1/juiced/library/pull", { repo, file, expected_generation: search.artifact_generation });
    await expect.poll(async () => (await service.artifacts()).artifacts.some((item: any) => item.id === artifact), { timeout: 15000 }).toBe(true);
  }
  async function cleanup(artifact: string) {
    await service.remove(added);
    await expect.poll(async () => (await service.artifacts()).jobs.every((job: any) => !job.writer_active), { timeout: 20000 }).toBe(true);
    await expect.poll(async () => {
      try { await service.removeArtifact(artifact); return true; } catch { return false; }
    }, { timeout: 15000 }).toBe(true);
  }
  return { service, sha, engines, cpuEngine, installEngine, pull, cleanup };
}

async function selectQuant(page: Page) {
  await open(page);
  await modelsView(page, "Discover");
  await result(page).getByRole("button", { name: /Q8_0/ }).click();
  const detail = page.getByRole("complementary", { name: "Model detail" });
  await expect(detail.getByRole("heading", { name: "Tiny-Chat-GGUF" })).toBeVisible();
  await expect(detail.getByRole("listbox", { name: "Quants" }).getByRole("option", { selected: true })).toContainText("Q8_0");
  return detail;
}

test("Download asks for the loading policy first, installs the engine when needed, adds the model and serves chat", async ({ page, request }) => {
  test.setTimeout(120000);
  const fixture = library(request);
  const artifact = await fixture.sha();
  await fixture.cleanup(artifact);
  const engineReady = !!fixture.cpuEngine(await fixture.engines());
  try {
    const detail = await selectQuant(page);
    await expect(result(page).getByText("✓ On this machine", { exact: true })).toHaveCount(0);
    await expect(detail.getByRole("radiogroup", { name: "Loading policy" }).getByRole("radio", { name: "On demand" })).toHaveAttribute("aria-checked", "true");
    await expect(detail.getByRole("button", { name: "Add model" })).toHaveCount(0);
    const pulling = page.waitForRequest((outgoing) => outgoing.method() === "POST" && new URL(outgoing.url()).pathname === "/v1/juiced/library/pull");
    await detail.getByRole("button", { name: "Download" }).click();
    expect((await pulling).postDataJSON()).toMatchObject({ repo, file, then: { policy: "on-demand", id: added } });
    await expect(page.getByRole("status").filter({ hasText: "Downloading Tiny-Chat-GGUF Q8_0. It's added as On demand when the download finishes." })).toBeVisible();
    await expect(detail.getByRole("status").filter({ hasText: `Added ${added}.` })).toBeVisible({ timeout: engineReady ? 20000 : 60000 });
    await expect(detail.getByRole("button", { name: "Add model" })).toHaveCount(0);
    await expect(detail.getByRole("option", { name: /Q8_0/ })).toContainText("✓ downloaded");
    if (!engineReady) expect(fixture.cpuEngine(await fixture.engines()), "the service installs the engine it picked").toBeTruthy();
    await expect(detail.getByRole("alert")).toHaveCount(0);
    expect(await fixture.service.model(added)).toMatchObject({ policy: "on-demand", loaded: false });

    await page.reload();
    await modelsView(page, "Downloads");
    const row = page.getByRole("list", { name: "Download list" }).getByRole("listitem", { name: "Tiny-Chat-GGUF Q8_0" });
    await expect(row.getByRole("button", { name: "Open model" }), "the finished add survives a reload").toBeVisible();
    await row.getByRole("button", { name: "Open model" }).click();
    await expect(page.getByRole("heading", { name: "Models", level: 1, exact: true })).toBeVisible();
    const inspector = page.getByRole("complementary", { name: "Model inspector" });
    await expect(inspector.getByRole("heading", { name: added, level: 2 })).toBeVisible();
    await expect(inspector.getByRole("radio", { name: "On demand" })).toHaveAttribute("aria-checked", "true");
    await closeInspector(page);
    await expect(page.getByLabel(`Loading policy for ${added}`)).toHaveValue("on-demand");

    await navigate(page, "Test services");
    await tryModel(page, added);
    const chat = page.getByRole("region", { name: "Chat test" });
    await expect(page.getByRole("region", { name: "Try a model" })).toContainText("Loads on first use");
    await chat.getByLabel("Prompt").fill("Engine journey");
    await chat.getByRole("button", { name: "Send" }).click();
    const reply = chat.getByRole("article", { name: "Model reply" });
    await expect(reply).toContainText("3 ", { timeout: 20000 });
    await expect(reply).toContainText("199", { timeout: 20000 });
    await expect(chat.getByRole("button", { name: "Stop" })).toHaveCount(0);
    expect((await fixture.service.model(added))!.loaded).toBe(true);
  } finally {
    await fixture.cleanup(artifact);
  }
  expect(await fixture.service.model(added)).toBeUndefined();
});

test("Add model with a name that is already registered reports it in the detail panel", async ({ page, request }) => {
  test.setTimeout(90000);
  const fixture = library(request);
  const artifact = await fixture.sha();
  await fixture.cleanup(artifact);
  await fixture.installEngine();
  await fixture.pull(artifact);
  const source = await fixture.service.source();
  try {
    const existing = await fixture.service.register({ id: added, model_file: source.model_file, command: source.command, args: source.args });
    const detail = await selectQuant(page);
    await expect(detail.getByRole("option", { name: /Q8_0/ })).toContainText("✓ downloaded");
    await detail.getByRole("radiogroup", { name: "Loading policy" }).getByRole("radio", { name: "Disabled" }).click();
    const answered = page.waitForResponse((response) => response.request().method() === "POST" && new URL(response.url()).pathname === "/v1/juiced/models/add");
    await detail.getByRole("button", { name: "Add model" }).click();
    expect((await answered).status()).toBe(409);
    expect((await answered).request().postDataJSON()).toMatchObject({ policy: "disabled" });
    await expect(detail.getByRole("alert")).toHaveText("A model with this name is already added. Rename or remove it under Models first.");
    await expect(detail.getByRole("button", { name: "Add model" })).toBeEnabled();
    await expect(detail.getByRole("button", { name: "Open model" })).toHaveCount(0);
    await expect(toasts(page).getByRole("alert")).toHaveCount(0);
    await expect(toasts(page).getByRole("status")).toHaveCount(0);
    expect(await fixture.service.model(added)).toMatchObject({ policy: "disabled", revision: existing.revision });
  } finally {
    await fixture.cleanup(artifact);
  }
});

test("a finished download offers Add model, then Open model once a model uses it", async ({ page, request }) => {
  test.setTimeout(90000);
  const fixture = library(request);
  const artifact = await fixture.sha();
  await fixture.cleanup(artifact);
  await fixture.installEngine();
  await fixture.pull(artifact);
  const search = await fixture.service.json<any>("/v1/juiced/library/search?q=tiny-chat&limit=12");
  await fixture.service.post("/v1/juiced/library/pull", { repo, file, expected_generation: search.artifact_generation });
  await expect.poll(async () => (await fixture.service.json<any>("/v1/juiced/library/pulls")).data.filter((pull: any) => pull.repo === repo && pull.state === "complete").length, { timeout: 15000 }).toBeGreaterThan(1);
  try {
    await open(page);
    await modelsView(page, "Downloads");
    const row = page.getByRole("list", { name: "Download list" }).getByRole("listitem", { name: "Tiny-Chat-GGUF Q8_0" });
    await expect(row, "repeated pulls of one quant share a row").toHaveCount(1);
    await row.getByRole("button", { name: "Add model" }).click();
    const details = page.getByRole("complementary", { name: "File details" });
    await expect(details.getByRole("listbox", { name: "Quants on disk" }).getByRole("option", { selected: true })).toContainText("Q8_0");
    await details.getByRole("button", { name: "Add model" }).click();
    await expect(details.getByRole("status").filter({ hasText: `Added ${added}.` })).toBeVisible({ timeout: 15000 });

    await modelsView(page, "Downloads");
    await row.getByRole("button", { name: "Open model" }).click();
    await expect(page.getByRole("complementary", { name: "Model inspector" }).getByRole("heading", { name: added, level: 2 })).toBeVisible();
  } finally {
    await fixture.cleanup(artifact);
  }
});
