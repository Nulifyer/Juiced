import type { APIRequestContext, Page } from "@playwright/test";
import { api, closeInspector, expect, modelsTab, modelsView, navigate, open, openModel, test, toasts, tryModel } from "./support";

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
  const installEngine = service.installEngine;
  async function cleanup(artifact: string) {
    await service.removePulls(repo);
    await service.removeModel(added);
    await expect.poll(async () => (await service.artifacts()).jobs.every((job: any) => !job.writer_active), { timeout: 20000 }).toBe(true);
    await expect.poll(async () => {
      try { await service.removeArtifact(artifact); return true; } catch { return false; }
    }, { timeout: 15000 }).toBe(true);
  }
  return { service, sha, engines, cpuEngine, installEngine, cleanup };
}

const downloads = (page: Page) => page.getByRole("list", { name: "Download list" }).getByRole("listitem", { name: "Tiny-Chat-GGUF Q8_0" });

async function selectQuant(page: Page) {
  await open(page);
  await modelsView(page, "Discover");
  await result(page).getByRole("button", { name: /Q8_0/ }).click();
  const detail = page.getByRole("complementary", { name: "Model detail" });
  await expect(detail.getByRole("heading", { name: "Tiny-Chat-GGUF" })).toBeVisible();
  await expect(detail.getByRole("listbox", { name: "Quants" }).getByRole("option", { selected: true })).toContainText("Q8_0");
  return detail;
}

test("Download asks for the loading policy first, installs the engine when needed, and the model is installed and serves chat", async ({ page, request }) => {
  test.setTimeout(120000);
  const fixture = library(request);
  const artifact = await fixture.sha();
  await fixture.cleanup(artifact);
  const engineReady = !!fixture.cpuEngine(await fixture.engines());
  try {
    const detail = await selectQuant(page);
    await expect(result(page).getByText("✓ On this machine", { exact: true })).toHaveCount(0);
    await expect(detail.getByRole("radiogroup", { name: "Loading policy" }).getByRole("radio", { name: "On demand" })).toHaveAttribute("aria-checked", "true");
    const pulling = page.waitForRequest((outgoing) => outgoing.method() === "POST" && new URL(outgoing.url()).pathname === "/v1/juiced/library/pull");
    await detail.getByRole("button", { name: "Download" }).click();
    expect((await pulling).postDataJSON()).toMatchObject({ repo, file, then: { policy: "on-demand", id: added } });
    await expect(page.getByRole("status").filter({ hasText: "Downloading Tiny-Chat-GGUF Q8_0. It's installed as On demand when done." })).toBeVisible();
    await expect(detail.getByRole("button", { name: "Open model" }), "the model exists when the download finishes").toBeVisible({ timeout: engineReady ? 20000 : 60000 });
    await expect(detail.getByRole("button", { name: /^(Add model|Install)$/ }), "there is no step after the download").toHaveCount(0);
    await expect(detail.getByRole("button", { name: "Download" })).toHaveCount(0);
    await expect(detail.getByRole("button", { name: "Remove", exact: true })).toBeEnabled();
    await expect(detail.getByRole("option", { name: /Q8_0/ })).toHaveText(/^Q8_0On demand.*KiB$/);
    await expect(detail.getByRole("button", { name: added, exact: true }), "Used by links the model").toBeVisible();
    if (!engineReady) expect(fixture.cpuEngine(await fixture.engines()), "the service installs the engine it picked").toBeTruthy();
    await expect(detail.getByRole("alert")).toHaveCount(0);
    expect(await fixture.service.model(added)).toMatchObject({ policy: "on-demand", loaded: false });

    await page.reload();
    await modelsView(page, "Downloads");
    await expect(page.getByRole("region", { name: "Downloads" }).getByRole("status")).toHaveCount(0);
    await expect(downloads(page), "a finished download leaves the list").toHaveCount(0);
    await expect(page.getByRole("button", { name: /^(Dismiss|Add model|Open model)$/ })).toHaveCount(0);
    await modelsView(page, "Installed");
    const inspector = await openModel(page, added);
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

test("a download whose model can't be made says why in Downloads and installs it on Retry", async ({ page, request }) => {
  test.setTimeout(90000);
  const fixture = library(request);
  const artifact = await fixture.sha();
  await fixture.cleanup(artifact);
  await fixture.installEngine();
  const source = await fixture.service.source();
  try {
    const existing = await fixture.service.register({ id: added, model_file: source.model_file, command: source.command, args: source.args });
    const detail = await selectQuant(page);
    await detail.getByRole("radiogroup", { name: "Loading policy" }).getByRole("radio", { name: "Disabled" }).click();
    await detail.getByRole("button", { name: "Download" }).click();
    await expect(detail.getByText("This model ID is already registered.")).toBeVisible({ timeout: 20000 });
    await expect(detail.getByRole("button", { name: "Retry" })).toBeEnabled();
    await expect(detail.getByRole("button", { name: /^(Add model|Install|Download)$/ })).toHaveCount(0);
    await expect(detail.getByRole("option", { name: /Q8_0/ }), "the quant is never Not added").toContainText("Not installed", { timeout: 15000 });
    expect(await fixture.service.model(added), "the model that holds the name is untouched").toMatchObject({ policy: "disabled", revision: existing.revision });

    await modelsView(page, "Downloads");
    const row = downloads(page);
    await expect(row).toContainText("Not installed");
    await expect(row).toContainText("This model ID is already registered.");
    await expect(row.getByRole("button")).toHaveText(["Retry", "Remove"]);
    await expect(modelsTab(page, "Downloads"), "a failed download is counted").toHaveText(/^Downloads \d+$/);
    await expect(toasts(page).getByRole("alert")).toHaveCount(0);

    await fixture.service.remove(added);
    await row.getByRole("button", { name: "Retry" }).click();
    await expect(row, "the download leaves once its model exists").toHaveCount(0, { timeout: 20000 });
    await expect.poll(async () => (await fixture.service.model(added))?.policy, { timeout: 10000 }).toBe("disabled");
    expect((await fixture.service.model(added))!.revision).not.toBe(existing.revision);
  } finally {
    await fixture.service.remove(added);
    await fixture.cleanup(artifact);
  }
});

test("a download started without a policy becomes a Disabled model, and Remove deletes its files", async ({ page, request }) => {
  test.setTimeout(150000);
  const fixture = library(request);
  const artifact = await fixture.sha();
  await fixture.cleanup(artifact);
  await fixture.installEngine();
  const search = await fixture.service.json<any>("/v1/juiced/library/search?q=tiny-chat&limit=12");
  await fixture.service.post("/v1/juiced/library/pull", { repo, file, expected_generation: search.artifact_generation });
  try {
    await expect.poll(async () => (await fixture.service.model(added))?.policy, { timeout: 90000, message: "the service registers the file by itself" }).toBe("disabled");
    expect((await fixture.service.json<any>("/v1/juiced/library/pulls")).data.filter((pull: any) => pull.repo === repo)).toEqual([]);
    await open(page);
    await modelsView(page, "Downloads");
    await expect(page.getByRole("region", { name: "Downloads" }).getByRole("status")).toHaveCount(0);
    await expect(downloads(page)).toHaveCount(0);

    await modelsView(page, "Installed");
    const inspector = await openModel(page, added);
    await inspector.getByRole("tab", { name: "Manage" }).click();
    await expect(inspector.getByRole("list", { name: "Model files" })).toContainText(file);
    await inspector.getByRole("button", { name: "Remove model", exact: true }).click();
    const confirm = page.getByRole("dialog", { name: `Remove ${added}?` });
    await expect(confirm).toContainText("Deletes 1 file and frees 96.0 KiB. Its settings go too.");
    await confirm.getByRole("button", { name: "Cancel" }).click();
    await expect(confirm).toHaveCount(0);
    expect(await fixture.service.model(added)).toBeTruthy();
    await inspector.getByRole("button", { name: "Remove model", exact: true }).click();
    await confirm.getByRole("button", { name: "Remove model", exact: true }).click();
    await expect(toasts(page).getByRole("status")).toContainText(`Removed ${added}. Freed 96.0 KiB.`);
    await expect(inspector).toHaveCount(0);
    expect(await fixture.service.model(added)).toBeUndefined();
    expect((await fixture.service.artifacts()).artifacts.some((item: any) => item.id === artifact), "its file is deleted").toBe(false);

    await modelsView(page, "Discover");
    await result(page).getByRole("button", { name: /Q8_0/ }).click();
    const detail = page.getByRole("complementary", { name: "Model detail" });
    await expect(detail.getByRole("button", { name: "Download" }), "a removed quant can be downloaded again").toBeEnabled();
    await expect(detail.getByRole("button", { name: "Open model" })).toHaveCount(0);
  } finally {
    await fixture.cleanup(artifact);
  }
});
