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
  await inspector.getByRole("radio", { name: "Disabled" }).click();
  await expect(toasts(page).getByRole("status")).toContainText(`Disabled and unloaded ${modelId}.`);
  expect(await service.model(modelId)).toMatchObject({ policy: "disabled", loaded: false });
  await inspector.getByRole("radio", { name: "Hot" }).click();
  await expect(toasts(page).getByRole("status")).toContainText(`${modelId} is now Hot.`);
  expect((await service.model(modelId))!.policy).toBe("hot");
  await inspector.getByRole("radio", { name: "On demand" }).click();
  await expect(toasts(page).getByRole("status")).toContainText(`${modelId} is now On demand.`);
  await sections.getByRole("tab", { name: "Settings" }).click();
  await expect(inspector.getByRole("region", { name: "Context and launch profile" })).toBeVisible();
  await expect(inspector.getByRole("region", { name: "Model runtime selection" })).toHaveCount(0);
  await inspector.getByRole("switch", { name: "Show advanced settings" }).click();
  await expect(inspector.getByRole("region", { name: "Model runtime selection" })).toBeVisible();
  await expect(inspector.getByRole("region", { name: "Context and launch profile" })).toContainText("Only llama.cpp models have launch settings here.");
  await sections.getByRole("tab", { name: "Manage" }).click();
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
    const context = panel.getByLabel("Context", { exact: true });
    const save = panel.getByRole("button", { name: "Save", exact: true });
    await expect(context).toHaveValue("1024");
    await expect(panel.getByLabel("Threads")).toHaveCount(0);
    await inspector.getByRole("switch", { name: "Show advanced settings" }).click();
    await expect(panel.getByLabel("Threads")).toHaveValue("2");
    await expect(panel.getByRole("table", { name: "Setting sources" })).toBeVisible();
    await expect(save).toHaveCount(0);
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
    await save.click();
    await expect(toasts(page).getByRole("alert")).toBeVisible();
    await expect(context).toHaveValue("2048");
    expect((await service.json<{ saved: { context_tokens: number } }>(path)).saved.context_tokens).toBe(3072);
    await panel.getByRole("button", { name: "Cancel", exact: true }).click();
    await expect(context).toHaveValue("3072");
    await expect(panel.getByLabel("Threads")).toHaveValue("3");

    await context.fill("4096");
    await panel.getByLabel("Threads").fill("");
    await save.click();
    await expect(toasts(page).getByRole("status")).toContainText(`Saved launch settings for ${id}.`);
    await expect(save).toHaveCount(0);
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

test("the memory estimate follows the context draft and blocks a save that doesn't fit", async ({ page, request }) => {
  const service = api(request);
  const fixture = await service.source();
  const id = `browser.fit-${test.info().project.name}`;
  await service.remove(id);
  await service.register({ id, model_file: fixture.model_file, provider: "llama-cpp", options: { context_tokens: 1024, threads: null, gpu_layers: 0, parallel_slots: 1 }, command: fixture.command, args: [...fixture.args, "--ctx-size", "1024"] });
  const gib = 1024 ** 3;
  const asked: string[] = [];
  await page.route(`**/v1/models/${encodeURIComponent(id)}/fit*`, (route) => {
    const context = Number(new URL(route.request().url()).searchParams.get("context_tokens") ?? 1024);
    asked.push(String(context));
    const total = 2 * gib + context * 1024 * 64;
    route.fulfill({ json: { schema_version: 1, model: id, source: "llama-fit-params", breakdown: { weights_bytes: 2 * gib, context_bytes: context * 1024 * 64, compute_bytes: 0, projector_bytes: 0, draft_bytes: 0, other_bytes: 0 }, total_bytes: total, budget_bytes: 8 * gib, available_bytes: 8 * gib, loaded: false, fits: total <= 8 * gib, largest_context_that_fits: 32768, trained_context: 131072, warnings: [] } });
  });
  try {
    await open(page);
    const inspector = await openModel(page, id);
    await tab(page, "Model sections", "Settings").click();
    const panel = inspector.getByRole("region", { name: "Context and launch profile" });
    const estimate = panel.getByRole("group", { name: "Memory estimate" });
    await expect(estimate).toContainText("2.1 GiB of 8.0 GiB available");
    const context = panel.getByLabel("Context", { exact: true });
    await panel.getByRole("button", { name: "Max" }).click();
    await expect(context).toHaveValue("131072");
    await expect(estimate).toContainText("doesn't fit");
    await expect(panel.getByRole("button", { name: "Save", exact: true })).toBeDisabled();
    await panel.getByRole("button", { name: "Auto" }).click();
    await expect(context).toHaveValue("32768");
    await expect(estimate).not.toContainText("doesn't fit");
    await expect(panel.getByRole("button", { name: "Save", exact: true })).toBeEnabled();
    expect(asked).toContain("131072");
    expect((await service.model(id))!.loaded).toBe(false);
  } finally {
    await service.remove(id);
  }
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
  const card = await source.elementHandle();
  await card!.dispatchEvent("dragstart", { dataTransfer: transfer });
  await target.dispatchEvent("dragover", { dataTransfer: transfer });
  await target.dispatchEvent("drop", { dataTransfer: transfer });
  await card!.dispatchEvent("dragend", { dataTransfer: transfer });
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

test("saving launch settings on a loaded model restarts it with the new settings", async ({ page, request }) => {
  const service = api(request);
  const fixture = await service.source();
  const id = `browser.reload-${test.info().project.name}`;
  const path = `/v1/models/${encodeURIComponent(id)}/options`;
  await service.remove(id);
  const options = { context_tokens: 1024, threads: 2, gpu_layers: 0, parallel_slots: 1 };
  await service.register({ id, model_file: fixture.model_file, provider: "llama-cpp", options, command: fixture.command, args: [...fixture.args, "--ctx-size", "1024"] });
  try {
    await service.policy(id, "on-demand");
    await service.post("/v1/load", { model: id });
    const before = (await service.model(id))!.pid;
    await open(page);
    const inspector = await openModel(page, id);
    await tab(page, "Model sections", "Settings").click();
    const panel = inspector.getByRole("region", { name: "Context and launch profile" });
    await inspector.getByRole("switch", { name: "Show advanced settings" }).click();
    await panel.getByLabel("Threads").fill("3");
    await expect(panel.getByRole("button", { name: "Save and reload" })).toBeEnabled();
    await panel.getByRole("button", { name: "Save and reload" }).click();
    await expect(toasts(page).getByRole("status")).toContainText(`Saved launch settings and restarted ${id}.`, { timeout: 15000 });
    expect((await service.json<{ saved: Record<string, number | null> }>(path)).saved).toMatchObject({ threads: 3 });
    await expect.poll(async () => { const now = await service.model(id); return now?.loaded && now.pid !== before; }, { timeout: 15000 }).toBe(true);
    expect((await service.model(id))?.policy).toBe("on-demand");
    await expect(panel.getByRole("button", { name: "Save and reload" })).toHaveCount(0);
  } finally {
    await service.remove(id);
  }
});

test("the engine section switches between installed engines and rolls back", async ({ page, request }) => {
  test.skip(process.platform === "win32", "The fixture installs engine packages on Unix only.");
  const service = api(request);
  const id = "fixture.managed-runtime";
  const path = `/v1/models/${encodeURIComponent(id)}/runtime`;
  const original = await service.json<{ current: string; revision: number }>(path);
  await open(page);
  const inspector = await openModel(page, hotModel);
  await tab(page, "Model sections", "Settings").click();
  await inspector.getByRole("switch", { name: "Show advanced settings" }).click();
  const hot = inspector.getByRole("region", { name: "Model runtime selection" });
  await expect(hot).toContainText("runs a command from its registration");
  await expect(hot.getByRole("button")).toHaveCount(1);

  const managed = await openModel(page, id);
  await tab(page, "Model sections", "Settings").click();
  const engine = managed.getByRole("region", { name: "Model runtime selection" });
  await expect(engine).toContainText("llama-cpp · fixture-runtime-first");
  await expect(engine.getByRole("button", { name: "Switch engine" })).toHaveCount(0);
  await engine.getByLabel("Switch to").selectOption({ label: "llama-cpp · fixture-runtime-second" });
  await expect(managed.getByRole("tab", { name: /^Settings/ }).getByLabel("unsaved")).toBeVisible();
  await engine.getByRole("button", { name: "Cancel" }).click();
  await expect(engine.getByLabel("Switch to")).toHaveValue("");
  await engine.getByLabel("Switch to").selectOption({ label: "llama-cpp · fixture-runtime-second" });
  await engine.getByRole("button", { name: "Switch engine" }).click();
  await expect(toasts(page).getByRole("status")).toContainText(`Changed the engine for ${id}.`);
  await expect(engine.getByRole("definition").first()).toContainText("fixture-runtime-second");
  await expect(engine).toContainText("Previous");
  expect((await service.json<{ current: string }>(path)).current).not.toBe(original.current);

  await engine.getByRole("button", { name: "Roll back to previous" }).click();
  await expect(toasts(page).getByRole("status")).toContainText(`Changed the engine for ${id}.`);
  await expect(engine.getByRole("definition").first()).toContainText("fixture-runtime-first");
  expect((await service.json<{ current: string }>(path)).current).toBe(original.current);
});
