import { existsSync } from "node:fs";
import type { Locator } from "@playwright/test";
import { api, closeInspector, expect, hotModel, modelId, navigate, open, openModel, rowFor, speechModel, tab, test, toasts } from "./support";

test("one search and service chips filter the model table", async ({ page }) => {
  await open(page);
  await navigate(page, "Models");
  const table = page.getByRole("table", { name: "Registered models" });
  const names = table.getByRole("button", { name: /^fixture\./ });
  await expect(names).toHaveCount(6);
  await expect(page.getByRole("searchbox")).toHaveCount(1);
  const chips = page.getByRole("group", { name: "Service filter" });
  await chips.getByRole("button", { name: /^Transcription/ }).click();
  await expect(chips.getByRole("button", { name: /^Transcription/ })).toHaveAttribute("aria-pressed", "true");
  await expect(names).toHaveCount(1);
  await expect(table.getByRole("button", { name: speechModel, exact: true })).toBeVisible();
  await chips.getByRole("button", { name: /^All/ }).click();
  await page.getByRole("searchbox", { name: "Search models" }).fill("long-context");
  await expect(names).toHaveCount(1);
  await expect(table.getByRole("button", { name: modelId, exact: true })).toBeVisible();
  await chips.getByRole("button", { name: /^Chat/ }).click();
  await expect(names).toHaveCount(1);
  await page.getByRole("searchbox", { name: "Search models" }).fill("no-such-model");
  await expect(page.getByRole("heading", { name: "No matches" })).toBeVisible();
  await page.getByRole("searchbox", { name: "Search models" }).fill("");
  await chips.getByRole("button", { name: /^Chat/ }).click();
  await expect(names).toHaveCount(6);
});

test("the inline policy select saves immediately and a stale revision fails without corrupting state", async ({ page, request }) => {
  const service = api(request);
  await open(page);
  await navigate(page, "Models");
  const select = page.getByLabel(`Loading policy for ${modelId}`);
  await select.selectOption("hot");
  await expect(toasts(page).getByRole("status")).toContainText(`${modelId} is now Hot.`);
  expect((await service.model(modelId))!.policy).toBe("hot");
  await expect(select).toHaveValue("hot");
  await select.selectOption("on-demand");
  await expect(toasts(page).getByRole("status")).toContainText(`${modelId} is now On demand.`);
  expect((await service.model(modelId))!.policy).toBe("on-demand");

  const stale = await (await request.get("/v1/juiced/catalog")).json();
  await page.route("**/v1/juiced/catalog", (route) => route.fulfill({ json: stale }));
  const changed = await service.policy(modelId, "disabled");
  await select.selectOption("hot");
  await expect(toasts(page).getByRole("alert")).toBeVisible();
  await expect(toasts(page).getByRole("status")).toHaveCount(0);
  const after = (await service.model(modelId))!;
  expect(after.policy).toBe("disabled");
  expect(after.revision).toBe(changed.revision);
  await page.unroute("**/v1/juiced/catalog");
  await expect(select).toHaveValue("disabled", { timeout: 10000 });
  await toasts(page).getByRole("button", { name: "Dismiss error" }).click();
  await expect(toasts(page).getByRole("alert")).toHaveCount(0);
});

test("row actions load and unload an On demand worker", async ({ page, request }) => {
  const service = api(request);
  await open(page);
  await navigate(page, "Models");
  const row = rowFor(page.getByRole("table", { name: "Registered models" }), modelId);
  const load = row.getByRole("button", { name: "Load", exact: true });
  test.skip(!(await load.isVisible()), "The phone layout moves Load and Unload into the inspector.");
  await expect(rowFor(page.getByRole("table", { name: "Registered models" }), hotModel).getByRole("button", { name: "Unload", exact: true })).toBeDisabled();
  await load.click();
  await expect(toasts(page).getByRole("status")).toContainText(`Loaded ${modelId}.`);
  expect((await service.model(modelId))!.loaded).toBe(true);
  await expect(row).toContainText("ready");
  await row.getByRole("button", { name: "Unload", exact: true }).click();
  await expect(toasts(page).getByRole("status")).toContainText(`Unloaded ${modelId}.`);
  expect((await service.model(modelId))!.loaded).toBe(false);
});

test("the inspector groups Overview, Settings and Manage tabs and loads from Overview", async ({ page, request }) => {
  const service = api(request);
  await open(page);
  const inspector = await openModel(page, modelId);
  const sections = inspector.getByRole("tablist", { name: "Model sections" });
  await expect(sections.getByRole("tab")).toHaveText(["Overview", "Settings", "Manage"]);
  await expect(sections.getByRole("tab", { name: "Overview" })).toHaveAttribute("aria-selected", "true");
  await expect(inspector.getByRole("radiogroup", { name: "Loading policy" }).getByRole("radio", { name: "On demand" })).toHaveAttribute("aria-checked", "true");
  await inspector.getByRole("button", { name: "Load worker" }).click();
  await expect(toasts(page).getByRole("status")).toContainText(`Loaded ${modelId}.`);
  await expect(inspector.getByRole("radio", { name: "Disabled" })).toBeDisabled();
  await inspector.getByRole("button", { name: "Unload", exact: true }).click();
  await expect(toasts(page).getByRole("status")).toContainText(`Unloaded ${modelId}.`);
  await inspector.getByRole("radio", { name: "Hot" }).click();
  await expect(toasts(page).getByRole("status")).toContainText(`${modelId} is now Hot.`);
  expect((await service.model(modelId))!.policy).toBe("hot");
  await inspector.getByRole("radio", { name: "On demand" }).click();
  await expect(toasts(page).getByRole("status")).toContainText(`${modelId} is now On demand.`);
  await sections.getByRole("tab", { name: "Settings" }).click();
  await expect(inspector.getByRole("region", { name: "Context and launch profile" })).toBeVisible();
  await expect(inspector.getByRole("region", { name: "Model runtime selection" })).toBeVisible();
  await expect(inspector.getByRole("region", { name: "Context and launch profile" })).toContainText("Only llama.cpp models have editable launch settings.");
  await sections.getByRole("tab", { name: "Manage" }).click();
  await expect(inspector.getByRole("region", { name: "Host fit" })).toBeVisible();
  await expect(inspector.getByRole("button", { name: "Remove registration" })).toBeDisabled();
  await expect(inspector).toContainText("Set the model to Disabled before removing its registration.");
  await closeInspector(page);
  await expect(inspector).toHaveCount(0);
});

test("launch settings drafts survive conflicts and guard navigation", async ({ page, request }) => {
  const service = api(request);
  const fixture = await service.source();
  const id = `browser.launch-${test.info().project.name}`;
  const path = `/v1/models/${encodeURIComponent(id)}/options`;
  await service.remove(id);
  const options = { context_tokens: 1024, threads: 2, gpu_layers: 0, parallel_slots: 2 };
  await service.register({ id, model_file: fixture.model_file, provider: "llama-cpp", options, command: fixture.command, args: [...fixture.args, "--parallel", "2", "--ctx-size", "1024"], max_inflight: 2 });
  try {
    await open(page);
    const inspector = await openModel(page, id);
    await tab(page, "Model sections", "Settings").click();
    const panel = inspector.getByRole("region", { name: "Context and launch profile" });
    const context = panel.getByLabel("Total context budget in tokens");
    await expect(context).toHaveValue("1024");
    await expect(panel.getByLabel("Threads")).toHaveValue("2");
    await expect(panel.getByRole("button", { name: "Save launch profile" })).toBeDisabled();
    await context.fill("2048");
    await expect(inspector.getByRole("tab", { name: /^Settings/ }).getByLabel("unsaved")).toBeVisible();

    await navigate(page, "Diagnostics", false);
    const discard = page.getByRole("dialog", { name: "Discard unsaved changes?" });
    await expect(discard).toBeVisible();
    await discard.getByRole("button", { name: "Keep editing" }).click();
    await expect(discard).toHaveCount(0);
    await expect(context).toHaveValue("2048");
    await expect(page.getByRole("heading", { name: "Models", level: 1 })).toBeVisible();

    const before = await service.json<{ revision: number }>(path);
    await service.post(path, { expected_revision: before.revision, options: { ...options, context_tokens: 3072, threads: 3 } });
    await expect(panel).toContainText("Another client changed this model. Your draft is kept", { timeout: 10000 });
    await expect(context).toHaveValue("2048");
    await panel.getByRole("button", { name: "Save launch profile" }).click();
    await expect(toasts(page).getByRole("alert")).toBeVisible();
    await expect(context).toHaveValue("2048");
    expect((await service.json<{ saved: { context_tokens: number } }>(path)).saved.context_tokens).toBe(3072);
    await panel.getByRole("button", { name: "Cancel launch draft" }).click();
    await expect(context).toHaveValue("3072");
    await expect(panel.getByLabel("Threads")).toHaveValue("3");

    await context.fill("4096");
    await panel.getByLabel("Threads").fill("");
    await panel.getByRole("button", { name: "Save launch profile" }).click();
    await expect(toasts(page).getByRole("status")).toContainText(`Saved launch settings for ${id}.`);
    await expect(panel.getByRole("button", { name: "Save launch profile" })).toBeDisabled();
    const saved = await service.json<{ saved: Record<string, number | null> }>(path);
    expect(saved.saved).toMatchObject({ context_tokens: 4096, threads: null, gpu_layers: 0 });
    expect((await service.model(id))).toMatchObject({ policy: "disabled", loaded: false });

    await context.fill("8192");
    await navigate(page, "Diagnostics", false);
    await discard.getByRole("button", { name: "Discard changes" }).click();
    await expect(page.getByRole("heading", { name: "Diagnostics", level: 1 })).toBeVisible();
    expect((await service.json<{ saved: { context_tokens: number } }>(path)).saved.context_tokens).toBe(4096);
  } finally {
    await service.remove(id);
  }
});

test("host fit checks the saved settings without reserving anything", async ({ page, request }) => {
  const service = api(request);
  const before = await service.json<{ resources: unknown }>("/v1/juiced/state");
  await open(page);
  const inspector = await openModel(page, modelId);
  await tab(page, "Model sections", "Manage").click();
  const fit = inspector.getByRole("region", { name: "Host fit" });
  await page.route("**/v1/juiced/suitability?*", (route) => route.fulfill({ status: 503, json: { error: { message: "Fixture check unavailable. Retry the check." } } }));
  await fit.getByRole("button", { name: "Check saved settings" }).click();
  await expect(fit.getByRole("alert")).toContainText("Fixture check unavailable");
  await page.unroute("**/v1/juiced/suitability?*");
  await fit.getByRole("button", { name: "Check saved settings" }).click();
  await expect(fit.getByRole("term").filter({ hasText: "Fits budget" })).toBeVisible();
  await expect(fit.getByRole("definition").filter({ hasText: /^(Yes|No|Unknown)$/ })).toHaveCount(1);
  await expect(fit).toContainText("This model needs");
  await expect(fit).toContainText("2.0 GiB");
  expect((await service.json<{ resources: unknown }>("/v1/juiced/state")).resources).toEqual(before.resources);
  expect((await service.model(modelId))!.loaded).toBe(false);
});

test("removing a registration keeps its weight file", async ({ page, request }) => {
  const service = api(request);
  const fixture = await service.source();
  const id = `browser.remove-${test.info().project.name}`;
  await service.remove(id);
  await service.register({ id, model_file: fixture.model_file, command: fixture.command, args: fixture.args });
  await open(page);
  const inspector = await openModel(page, id);
  await tab(page, "Model sections", "Manage").click();
  await inspector.getByRole("button", { name: "Remove registration" }).click();
  await expect(toasts(page).getByRole("status")).toContainText(`Removed ${id}. Weight files kept.`);
  await expect(inspector).toHaveCount(0);
  await expect(page.getByRole("table", { name: "Registered models" }).getByRole("button", { name: id, exact: true })).toHaveCount(0);
  expect(await service.model(id)).toBeUndefined();
  expect(existsSync(fixture.model_file)).toBe(true);
});

async function drag(source: Locator, target: Locator) {
  if (!test.info().project.use.hasTouch) return source.dragTo(target);
  const transfer = await source.page().evaluateHandle(() => new DataTransfer());
  await source.dispatchEvent("dragstart", { dataTransfer: transfer });
  await target.dispatchEvent("dragover", { dataTransfer: transfer });
  await target.dispatchEvent("drop", { dataTransfer: transfer });
  await source.dispatchEvent("dragend", { dataTransfer: transfer }).catch(() => {});
}

test.describe("Loading policy board", () => {
  test("dragging a card to another lane changes its policy", async ({ page, request }) => {
    const service = api(request);
    await open(page);
    await navigate(page, "Loading policy");
    const lanes = ["Hot models", "On demand models", "Disabled models"].map((name) => page.getByRole("region", { name }));
    for (const lane of lanes) await expect(lane).toBeVisible();
    await expect(lanes[0]!.getByRole("button", { name: new RegExp(hotModel) })).toBeVisible();
    const card = lanes[1]!.getByRole("button", { name: new RegExp(modelId) });
    await expect(card).toBeVisible();
    await drag(card, lanes[2]!);
    await expect(toasts(page).getByRole("status")).toContainText(`${modelId} is now Disabled.`);
    await expect(lanes[2]!.getByRole("button", { name: new RegExp(modelId) })).toBeVisible();
    expect((await service.model(modelId))!.policy).toBe("disabled");
    await drag(lanes[2]!.getByRole("button", { name: new RegExp(modelId) }), lanes[1]!);
    await expect(toasts(page).getByRole("status")).toContainText(`${modelId} is now On demand.`);
    expect((await service.model(modelId))!.policy).toBe("on-demand");
  });

  test("the per-card select changes policy from the keyboard", async ({ page, request }) => {
    const service = api(request);
    await open(page);
    await navigate(page, "Loading policy");
    const select = page.getByRole("region", { name: "On demand models" }).getByLabel(`Loading policy for ${modelId}`);
    await select.focus();
    await page.keyboard.press("ArrowDown");
    await expect(toasts(page).getByRole("status")).toContainText(`${modelId} is now Disabled.`);
    expect((await service.model(modelId))!.policy).toBe("disabled");
    const moved = page.getByRole("region", { name: "Disabled models" }).getByLabel(`Loading policy for ${modelId}`);
    await expect(moved).toHaveValue("disabled");
    await moved.focus();
    await page.keyboard.press("ArrowUp");
    await expect(toasts(page).getByRole("status")).toContainText(`${modelId} is now On demand.`);
    expect((await service.model(modelId))!.policy).toBe("on-demand");
    await page.getByRole("region", { name: "On demand models" }).getByRole("button", { name: new RegExp(modelId) }).click();
    await expect(page.getByRole("complementary", { name: "Model inspector" }).getByRole("heading", { name: modelId })).toBeVisible();
  });
});
