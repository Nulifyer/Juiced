import type { APIRequestContext, Page } from "@playwright/test";
import { api, closeInspector, expect, navigate, open, tab, test, toasts } from "./support";

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
    await service.removeArtifact(artifact);
  }
  return { service, sha, engines, cpuEngine, installEngine, pull, cleanup };
}

async function selectQuant(page: Page) {
  await open(page);
  await navigate(page, "Library");
  await expect(tab(page, "Library view", "Find models")).toHaveAttribute("aria-selected", "true");
  await result(page).getByRole("button", { name: /Q8_0/ }).click();
  const detail = page.getByRole("complementary", { name: "Model detail" });
  await expect(detail.getByRole("heading", { name: "Tiny-Chat-GGUF" })).toBeVisible();
  await expect(detail.getByRole("listbox", { name: "Quants" }).getByRole("option", { selected: true })).toContainText("Q8_0");
  return detail;
}

test("Add model downloads a quant, installs the engine when needed and serves chat", async ({ page, request }) => {
  test.setTimeout(120000);
  const fixture = library(request);
  const artifact = await fixture.sha();
  await fixture.cleanup(artifact);
  const engineReady = !!fixture.cpuEngine(await fixture.engines());
  try {
    const detail = await selectQuant(page);
    await expect(result(page).getByText("On this machine", { exact: true })).toHaveCount(0);
    await detail.getByRole("button", { name: "Download" }).click();
    await expect(page.getByRole("status").filter({ hasText: "Downloading Tiny-Chat-GGUF Q8_0." })).toBeVisible();
    await expect(result(page).getByText("On this machine", { exact: true })).toBeVisible({ timeout: 20000 });
    await expect(detail.getByRole("option", { name: /Q8_0/ })).toContainText("✓ downloaded");
    await expect(detail.getByRole("button", { name: "Download" })).toHaveCount(0);

    const adding = page.waitForRequest((outgoing) => outgoing.method() === "POST" && new URL(outgoing.url()).pathname === "/v1/juiced/models/add");
    await detail.getByRole("button", { name: "Add model" }).click();
    expect((await adding).postDataJSON()).toEqual({ artifact, policy: "on-demand" });
    const done = detail.getByRole("status").filter({ hasText: `Added ${added}. It runs on CPU and loads on its first request.` });
    if (engineReady) {
      await expect(done).toBeVisible({ timeout: 15000 });
      await expect(detail.getByRole("button", { name: "Install engine and add" })).toHaveCount(0);
    } else {
      await expect(detail).toContainText(/This model needs the llama\.cpp CPU engine( \([^)]+ download\))?\. Juiced picked it for this machine\./);
      await expect(detail.getByRole("alert")).toHaveCount(0);
      expect(await fixture.service.model(added)).toBeUndefined();
      const installing = page.waitForRequest((outgoing) => outgoing.method() === "POST" && new URL(outgoing.url()).pathname === "/v1/juiced/engines/install");
      await detail.getByRole("button", { name: "Install engine and add" }).click();
      expect((await installing).postDataJSON()).toMatchObject({ engine: "llama.cpp", backend: "cpu" });
      await expect(detail.getByRole("progressbar", { name: "Engine install progress" })).toBeVisible();
      await expect(detail).toContainText(/Installing engine… (queued|downloading|extracting|installing|complete)/);
      await expect(done).toBeVisible({ timeout: 30000 });
      await expect(detail.getByRole("progressbar", { name: "Engine install progress" })).toHaveCount(0);
      expect(fixture.cpuEngine(await fixture.engines())).toBeTruthy();
    }
    await expect(detail.getByRole("alert")).toHaveCount(0);
    expect(await fixture.service.model(added)).toMatchObject({ policy: "on-demand", loaded: false });

    await detail.getByRole("button", { name: "Open model" }).click();
    await expect(page.getByRole("heading", { name: "Models", level: 1, exact: true })).toBeVisible();
    const inspector = page.getByRole("complementary", { name: "Model inspector" });
    await expect(inspector.getByRole("heading", { name: added, level: 2 })).toBeVisible();
    await expect(inspector.getByRole("radio", { name: "On demand" })).toHaveAttribute("aria-checked", "true");
    await closeInspector(page);
    await expect(page.getByLabel(`Loading policy for ${added}`)).toHaveValue("on-demand");

    await navigate(page, "Test services");
    const chat = page.getByRole("region", { name: "Chat test" });
    await chat.getByLabel("Model").selectOption(added);
    await expect(chat).toContainText("Loads on first message");
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
    const answered = page.waitForResponse((response) => response.request().method() === "POST" && new URL(response.url()).pathname === "/v1/juiced/models/add");
    await detail.getByRole("button", { name: "Add model" }).click();
    expect((await answered).status()).toBe(409);
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
