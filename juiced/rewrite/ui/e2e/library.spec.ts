import type { Locator, Page } from "@playwright/test";
import { existsSync } from "node:fs";
import { mkdir, symlink, writeFile } from "node:fs/promises";
import { dirname, relative, resolve } from "node:path";
import { api, closeInspector, expect, largeText, modelsTab, modelsView, noHorizontalOverflow, open, openModel, showAllModels, test } from "./support";

const result = (page: Page, name: RegExp) => page.getByRole("list", { name: "Search results" }).getByRole("listitem").filter({ has: page.getByText(name) });
const suggestions = (page: Page) => page.getByRole("list", { name: "Suggested models" });
const searchBox = (page: Page) => page.getByRole("searchbox", { name: "Search models" });
const GiB = 1024 ** 3;

async function discover(page: Page) {
  await open(page);
  await modelsView(page, "Discover");
}

/** Discover with a search for `text`, once the Qwen3 0.6B result is listed. */
async function findModels(page: Page, text = "") {
  await discover(page);
  if (text) await searchBox(page).fill(text);
  else await page.getByRole("combobox", { name: "Type" }).selectOption({ label: "Chat" });
  await expect(result(page, /^Qwen3 0\.6B$/)).toBeVisible({ timeout: 15000 });
}

type Fake = { id: string; name: string; task: string; gib: number; label?: string; present?: boolean; source?: string; downloads?: number; repo?: string };
const fakeModel = ({ id, name, task, gib, label = "Q4_K_M", present = false, source = "catalog", downloads, repo = `fixture/${id}` }: Fake) => ({
  id, repo, name, publisher: "fixture", task, source, downloads: downloads ?? null, gated: false,
  quants: [
    { label: "Q2_K", kind: "quant", file: `${id}-Q2_K.gguf`, parts: 1, bytes: Math.round(gib * GiB / 2), recommended: false, pullable: true, sha256: null, local: { present: false }, jobs: [] },
    { label, kind: "quant", file: `${id}-${label}.gguf`, parts: 1, bytes: Math.round(gib * GiB), recommended: source === "catalog", pullable: true, sha256: null, local: { present }, jobs: [] },
  ],
});
const fakePage = (data: unknown[], next: string | null = null) => ({ schema_version: 1, artifact_generation: 1, offline: false, sources: { huggingface: { status: "ok", authenticated: false } }, data, page: { limit: 12, next_cursor: next } });
/** The fixture machine has 31 GiB of memory and no GPU: 15.6 GiB for the first chat model, 3.9 GiB for the rest. */
const catalog: Fake[] = [
  { id: "chat-30b", name: "Chat 30B", task: "chat", gib: 17.3 }, { id: "chat-14b", name: "Chat 14B", task: "chat", gib: 8 }, { id: "chat-12b", name: "Chat 12B", task: "chat", gib: 12, present: true },
  { id: "chat-4b", name: "Chat 4B", task: "chat", gib: 2.7, label: "UD-Q4_K_XL" }, { id: "chat-1b", name: "Chat 1B", task: "chat", gib: 1 },
  { id: "embed-small", name: "Embed Small", task: "embeddings", gib: 0.6, label: "Q8_0" }, { id: "embed-large", name: "Embed Large", task: "embeddings", gib: 7.5, label: "Q8_0" },
  { id: "speech-turbo", name: "Speech Turbo", task: "transcription", gib: 1.5 }, { id: "speech-large", name: "Speech Large", task: "transcription", gib: 3.1 },
  { id: "voice", name: "Voice", task: "tts", gib: 0.3 }, { id: "picture", name: "Picture", task: "image", gib: 1.9, label: "F16" },
];

/** Answers Discover's requests from fixed data; `hub` supplies each page of a search by its cursor. */
async function fakeLibrary(page: Page, hub: (cursor: string | null, url: URL) => Promise<unknown> | unknown = () => fakePage([])) {
  const requests: string[] = [];
  await page.route("**/v1/juiced/library/search*", async (route) => {
    const url = new URL(route.request().url());
    requests.push(url.search);
    if (url.searchParams.get("source") === "catalog") return route.fulfill({ json: fakePage(catalog.filter((model) => model.task === url.searchParams.get("task")).map(fakeModel)) });
    return route.fulfill({ json: await hub(url.searchParams.get("cursor"), url) });
  });
  return requests;
}

/** Removes a model a download added and its file, once the download and its add have settled. */
async function removeDownloaded(service: ReturnType<typeof api>, model: string, sha: () => Promise<string>) {
  await expect.poll(async () => (await service.artifacts()).jobs.every((job: any) => !job.writer_active), { timeout: 20000 }).toBe(true);
  await expect.poll(async () => (await service.json<any>("/v1/juiced/library/pulls")).data.every((pull: any) => pull.then?.state !== "pending"), { timeout: 60000 }).toBe(true);
  await expect.poll(async () => { try { await service.remove(model); await service.removeArtifact(await sha()); return true; } catch { return false; } }, { timeout: 15000 }).toBe(true);
}

test.describe("Discover", () => {
  test("opens on a short list of suggestions for this machine, each with one quant and one Download", async ({ page }, info) => {
    const requests = await fakeLibrary(page);
    await discover(page);
    const rows = suggestions(page).getByRole("listitem");
    await expect(rows, "two chat models, then one of each other type; what is on disk or too large is left out").toHaveText([/^Chat 14B/, /^Chat 4B/, /^Embed Small/, /^Speech Large/, /^Voice/, /^Picture/]);
    await expect(page.getByRole("heading", { name: "Suggested for this machine" })).toBeVisible();
    await expect(rows.nth(0)).toContainText("fixture · Chat");
    await expect(rows.nth(0).locator(".result-quant")).toHaveText(/^Q4_K_M\s*8\.0 GiB$/);
    await expect(rows.nth(0).locator(".result-quant").getByRole("img")).toHaveAccessibleName("Runs on the CPU");
    await expect(rows.nth(1).locator(".result-quant")).toContainText("UD-Q4_K_XL");
    await expect(rows.nth(3)).toContainText("Speech to text");
    for (const row of await rows.all()) await expect(row.getByRole("button", { name: /^Download / })).toBeEnabled();
    await expect(rows.getByText("Recommended"), "every suggestion is recommended, so no row says it").toHaveCount(0);
    await expect(page.getByRole("list", { name: "Search results" })).toHaveCount(0);
    await expect(page.getByRole("button", { name: "Clear filters" })).toHaveCount(0);
    await expect(page.getByRole("combobox", { name: "Sort" }), "sort orders search results only").toBeDisabled();
    expect(requests.filter((query) => !query.includes("source=catalog")), "no catalog or hub search until the user asks").toEqual([]);
    if (info.project.name !== "phone") expect(await page.locator("main").evaluate((main) => main.scrollHeight <= main.clientHeight), "the suggestions fit one screen").toBe(true);
    await page.getByRole("button", { name: "How suggestions are chosen" }).click();
    await expect(page.getByText("For each type, the largest recommended model that fits.")).toBeVisible();
    await page.keyboard.press("Escape");

    await rows.nth(1).getByRole("button", { name: "2 quants" }).click();
    const detail = page.getByRole("complementary", { name: "Model detail" });
    await expect(detail.getByRole("listbox", { name: "Quants" }).getByRole("option", { selected: true })).toContainText("UD-Q4_K_XL");
    await noHorizontalOverflow(page, "Discover suggestions");
  });

  test("a suggestion downloads in one step, keeps its row and then leads to the model", async ({ page, request }) => {
    test.setTimeout(120000);
    const service = api(request);
    const added = "Qwen3-1.7B-Q4_0";
    const sha = async () => (await service.json<any>("/v1/juiced/library/search?source=catalog&task=chat&limit=20")).data.find((model: any) => model.id === "qwen3-1.7b").quants.find((quant: any) => quant.label === "Q4_0").sha256 as string;
    const cleanup = () => removeDownloaded(service, added, sha);
    await cleanup();
    try {
      await discover(page);
      const row = suggestions(page).getByRole("listitem").filter({ has: page.getByText("Qwen3 1.7B", { exact: true }) });
      await expect(row.locator(".result-quant")).toContainText("Q4_0");
      await expect(suggestions(page).getByRole("listitem").filter({ hasText: "Qwen3 0.6B" }), "a model with a quant on disk is not suggested").toHaveCount(0);
      const top = async () => (await row.boundingBox())!.y;
      const before = await top();
      const pulling = page.waitForRequest((outgoing) => outgoing.method() === "POST" && new URL(outgoing.url()).pathname === "/v1/juiced/library/pull");
      await row.getByRole("button", { name: "Download Qwen3 1.7B Q4_0" }).click();
      expect((await pulling).postDataJSON()).toMatchObject({ repo: "unsloth/Qwen3-1.7B-GGUF", file: "Qwen3-1.7B-Q4_0.gguf", then: { policy: "on-demand", id: added } });
      await expect(page.getByRole("status").filter({ hasText: "Downloading Qwen3 1.7B Q4_0. It's added as On demand when done." })).toBeVisible();
      await expect(row.getByRole("button", { name: `Open model ${added}` })).toBeVisible({ timeout: 60000 });
      await expect(row.getByText("✓ On disk", { exact: true })).toBeVisible();
      await expect(row.getByRole("button", { name: /^Download/ }), "a model on disk is not offered as a download").toHaveCount(0);
      expect(await top(), "the row stays where it was through the download").toBe(before);
      await expect(suggestions(page).getByRole("listitem"), "a finished download does not reshuffle the suggestions").toHaveCount(2);
      await row.getByRole("button", { name: `Open model ${added}` }).click();
      await expect(page.getByRole("complementary", { name: "Model inspector" }).getByRole("heading", { name: added, level: 2 })).toBeVisible();
    } finally { await cleanup(); }
  });

  test("search or any filter replaces the suggestions with service results; clearing brings them back", async ({ page }) => {
    await discover(page);
    await expect(suggestions(page).getByRole("listitem").first()).toBeVisible({ timeout: 15000 });
    await expect(page.getByRole("button", { name: /refresh/i })).toHaveCount(0);
    await searchBox(page).fill("qwen3");
    const qwen = result(page, /^Qwen3 0\.6B$/);
    await expect(qwen).toBeVisible({ timeout: 15000 });
    await expect(suggestions(page)).toHaveCount(0);
    await expect(qwen.getByText("Recommended", { exact: true })).toBeVisible();
    await expect(qwen).toContainText("unsloth · Chat");
    await expect(qwen.getByText("✓ On disk", { exact: true })).toBeVisible();
    await expect(qwen.locator(".result-quant"), "the quant on disk is the one shown").toContainText("Q8_0");
    await expect(qwen.getByRole("button", { name: "11 quants" })).toBeVisible();
    await expect(qwen.getByRole("button", { name: /^Download/ }), "a model on disk leads to it, not to a download").toHaveCount(0);

    await searchBox(page).fill("tiny-chat");
    const tiny = result(page, /^Tiny-Chat-GGUF$/);
    await expect(page.getByRole("list", { name: "Search results" }).getByRole("listitem")).toHaveCount(1);
    await expect(tiny).toContainText("fixture-labs · Chat · 4k downloads");
    await expect(tiny.getByText("Recommended", { exact: true })).toHaveCount(0);
    await expect(tiny.locator(".result-quant")).toContainText("Q4_K_M");
    await expect(tiny.getByRole("button", { name: "Download Tiny-Chat-GGUF Q4_K_M" })).toBeEnabled();
    await searchBox(page).fill("no-such-model-anywhere");
    await expect(page.getByRole("heading", { name: "No models found" })).toBeVisible();
    await expect(page.getByText("Try a shorter search.")).toBeVisible();
    await searchBox(page).fill("");
    await expect(suggestions(page).getByRole("listitem").first(), "no search and no filter: suggestions again").toBeVisible();
    await expect(page.getByRole("list", { name: "Search results" })).toHaveCount(0);

    const type = page.getByRole("combobox", { name: "Type" });
    const searched = page.waitForRequest((outgoing) => { const url = new URL(outgoing.url()); return url.pathname === "/v1/juiced/library/search" && url.searchParams.get("task") === "embeddings" && !url.searchParams.has("source") && !url.searchParams.has("q"); });
    await type.selectOption({ label: "Embeddings" });
    await searched;
    await expect(type).toHaveAttribute("data-active", "true");
    await expect(result(page, /^Nomic Embed Text v1$/)).toBeVisible();
    await expect(result(page, /^Qwen3 Embedding 8B$/), "a filter searches the whole catalog, not the suggestions").toBeVisible();
    await expect(qwen).toHaveCount(0);
    await expect(tiny).toHaveCount(0);
    await type.selectOption({ label: "Speech to text" });
    await expect(result(page, /^Whisper Tiny$/)).toBeVisible();
    await expect(result(page, /^Whisper Tiny$/).locator(".result-meta"), "result lines use the type names").toContainText("Speech to text");
    await expect(result(page, /^Nomic Embed Text v1$/)).toHaveCount(0);
    await page.getByRole("button", { name: "Clear filters" }).click();
    await expect(type).toHaveValue("");
    await expect(suggestions(page).getByRole("listitem").first()).toBeVisible();
  });

  test("hardware, size and sort narrow the same results and clear in one step", async ({ page }) => {
    await discover(page);
    const clear = page.getByRole("button", { name: "Clear filters" });
    await expect(clear, "nothing to clear without a filter").toHaveCount(0);
    const type = page.getByRole("combobox", { name: "Type" });
    await type.selectOption({ label: "Chat" });
    const results = page.getByRole("list", { name: "Search results" }).getByRole("listitem");
    const qwen = result(page, /^Qwen3 0\.6B$/), offered = qwen.locator(".result-quant");
    await expect(qwen).toBeVisible({ timeout: 15000 });
    const count = page.locator(".result-line").getByRole("status");
    await expect(page.getByText("Searching Hugging Face…")).toHaveCount(0);
    await expect(count).toHaveText(new RegExp(`^${await results.count()} models$`));
    const top = (await qwen.boundingBox())!.y, bar = await page.locator(".find-filters").boundingBox();

    const size = page.getByRole("combobox", { name: "Size" }), hardware = page.getByRole("combobox", { name: "Hardware" });
    await expect(hardware.getByRole("option", { name: "NPU" }), "no NPU choice without FastFlowLM").toHaveCount(0);
    await size.selectOption({ label: "Over 24 GiB" });
    await expect(result(page, /^Tiny-Chat-GGUF$/), "a model with no quant that large is dropped").toHaveCount(0);
    await expect(offered, "the row offers a quant that matches").toContainText("BF16");
    await expect(qwen.getByRole("button", { name: "View Qwen3 0.6B BF16" }), "a quant too big for this machine is not a one-step download").toBeVisible();
    await expect(count).toHaveText(new RegExp(`^${await results.count()} models?$`));
    expect((await qwen.boundingBox())!.y, "a filter moves no result down").toBe(top);
    await hardware.selectOption({ label: "Fits memory" });
    await expect(qwen, "filters combine: its only large quant does not fit").toHaveCount(0);
    await expect(size).toHaveAttribute("data-active", "true");
    await expect(hardware).toHaveAttribute("data-active", "true");
    await type.selectOption({ label: "Classify" });
    await expect(page.getByRole("heading", { name: "No models found" })).toBeVisible();
    await expect(page.getByText("No model matches every filter.")).toBeVisible();
    await type.selectOption("");
    await size.selectOption({ label: "Under 2 GiB" });
    await expect(offered).toContainText("KiB");
    await expect(offered).not.toContainText("BF16");
    await noHorizontalOverflow(page, "Discover with filters");
    expect(await page.locator(".find-filters").boundingBox(), "filters move nothing in the bar").toEqual(bar);

    await clear.click();
    await expect(size).toHaveValue("");
    await expect(hardware).toHaveValue("");
    await expect(clear).toHaveCount(0);
    await expect(suggestions(page).getByRole("listitem").first()).toBeVisible();

    const sort = page.getByRole("combobox", { name: "Sort" });
    await type.selectOption({ label: "Chat" });
    await sort.selectOption({ label: "Most downloads" });
    await expect(results.first()).toContainText("Qwen3-0.6B");
    await expect(results.first()).toContainText("9.8M downloads");
    await clear.click();
    await expect(sort, "sorting is not a filter: clearing keeps it").toHaveValue("downloads");
    await expect(clear).toHaveCount(0);
  });

  test("a narrow filter says results stop at the loaded pages and offers more", async ({ page }) => {
    const models = (from: number, count: number, gib: number) => Array.from({ length: count }, (_, index) => fakeModel({ id: `hub-${from + index}`, name: `Hub Model ${from + index}`, task: "chat", gib, source: "huggingface", downloads: 1000 - from - index }));
    const pages: Record<string, unknown> = { "": fakePage(models(1, 12, 30), "page-2"), "page-2": fakePage([...models(13, 11, 30), fakeModel({ id: "hub-small", name: "Hub Small", task: "chat", gib: 1, source: "huggingface" })], "page-3"), "page-3": fakePage(models(24, 3, 30)) };
    const requests = await fakeLibrary(page, (cursor) => pages[cursor ?? ""]);
    await discover(page);
    await page.getByRole("combobox", { name: "Type" }).selectOption({ label: "Chat" });
    const results = page.getByRole("list", { name: "Search results" }).getByRole("listitem");
    const count = page.locator(".result-line").getByRole("status");
    await expect(results).toHaveCount(12);
    await expect(count, "a cut list says so").toHaveText("First 12 models");
    await page.getByRole("combobox", { name: "Size" }).selectOption({ label: "Under 2 GiB" });
    await expect(page.getByRole("heading", { name: "No match in the first 12 models" }), "an empty page is not the end of the search").toBeVisible();
    await page.getByRole("button", { name: "Search more" }).click();
    await expect(results).toHaveText([/^Hub Small/]);
    await expect(count).toHaveText("1 of the first 24 models match");
    expect(requests.at(-1), "the next page keeps the filter").toContain("task=chat");
    expect(requests.at(-1)).toContain("cursor=page-2");
    await page.getByRole("button", { name: "Show more" }).click();
    await expect(count, "the last page ends the list").toHaveText("1 model");
    await expect(page.getByRole("button", { name: "Show more" })).toHaveCount(0);
    await page.getByRole("button", { name: "Clear filters" }).click();
    await page.getByRole("combobox", { name: "Type" }).selectOption({ label: "Chat" });
    await expect(results, "a new search starts from the first page").toHaveCount(12);
    await page.getByRole("button", { name: "Show more" }).click();
    await expect(results).toHaveCount(24);
    await expect(count).toHaveText("First 24 models");
  });

  test("the list is one column and the search bar stays put in every state and width", async ({ page }, info) => {
    test.setTimeout(120000);
    let wait = 0;
    const many = Array.from({ length: 30 }, (_, index) => fakeModel({ id: `hub-${index}`, name: index % 7 === 0 ? `Hub-Model-With-A-Long-Repository-Name-${index}-GGUF` : `Hub Model ${index}`, task: "chat", gib: 1 + index, source: index % 5 === 0 ? "catalog" : "huggingface", present: index === 3, downloads: 5000 * index }));
    await fakeLibrary(page, async (_, url) => {
      if (wait) await new Promise((done) => setTimeout(done, wait));
      const text = url.searchParams.get("q") ?? "";
      return fakePage(text === "none" ? [] : text === "few" ? many.slice(0, 2) : many);
    });
    await discover(page);
    const bar = page.locator(".find-filters"), list = page.locator(".model-results");
    const layout = () => page.evaluate(() => {
      const box = (element: Element) => { const rect = element.getBoundingClientRect(); return [rect.x, rect.y, rect.width, rect.height].map(Math.round).join(","); };
      const controls = [...document.querySelectorAll(".find-filters > input[type=search], .find-filters > select, .find-filters > .menu-trigger")];
      return {
        bar: box(document.querySelector(".find-filters")!), controls: controls.map((control) => `${control.getAttribute("aria-label")}@${box(control)}`),
        columns: new Set([...document.querySelectorAll(".model-results > li")].filter((item) => item.getClientRects().length).map((item) => Math.round(item.getBoundingClientRect().x))).size,
        inOrder: controls.every((control, index) => { if (!index) return true; const before = controls[index - 1]!.getBoundingClientRect(), now = control.getBoundingClientRect(); return now.y > before.y || (now.y === before.y && now.x > before.x); }),
      };
    });
    const widths = info.project.name === "desktop" ? [1920, 1440, 1100, 900, 768, 390] : [page.viewportSize()!.width];
    for (const width of widths) {
      await page.setViewportSize({ width, height: 900 });
      const type = page.getByRole("combobox", { name: "Type" }), detail = page.getByRole("complementary", { name: "Model detail" });
      const closeDetail = async () => { const back = detail.getByRole("button", { name: "Back to results" }); await (await back.isVisible() ? back : detail.getByRole("button", { name: "Close detail" })).click(); await expect(detail).toHaveCount(0); };
      await expect(suggestions(page).getByRole("listitem")).toHaveCount(6);
      const states: [string, Awaited<ReturnType<typeof layout>>][] = [["suggestions", await layout()]];
      const record = async (state: string) => { states.push([state, await layout()]); };
      await suggestions(page).getByRole("listitem").first().getByRole("button", { name: "Chat 14B", exact: true }).click();
      await expect(detail).toBeVisible();
      await record("suggestions with the detail open");
      await closeDetail();
      wait = 600;
      await type.selectOption({ label: "Chat" });
      await expect(page.locator(".result-line").getByRole("status")).toHaveText("Searching…");
      await record("loading");
      await expect(list.getByRole("listitem")).toHaveCount(30);
      wait = 0;
      await record("many results with a filter");
      await list.getByRole("listitem").nth(1).getByRole("button", { name: "Hub Model 1", exact: true }).click();
      await expect(detail).toBeVisible();
      await record("many results with the detail open");
      await closeDetail();
      await searchBox(page).fill("few");
      await expect(list.getByRole("listitem")).toHaveCount(2);
      await record("few results");
      await page.getByRole("combobox", { name: "Size" }).selectOption({ label: "Over 24 GiB" });
      await page.getByRole("combobox", { name: "Hardware" }).selectOption({ label: "Fits memory" });
      await expect(page.getByRole("heading", { name: "No models found" })).toBeVisible();
      await record("every filter set and nothing found");
      await searchBox(page).fill("none");
      await page.getByRole("button", { name: "Clear filters" }).click();
      await expect(page.getByText("Try a shorter search.")).toBeVisible();
      await record("no results");
      await searchBox(page).fill("");
      const [, first] = states[0]!;
      for (const [state, measured] of states) {
        expect(measured.bar, `${width}px, ${state}: the search bar keeps its place and size`).toBe(first.bar);
        expect(measured.controls, `${width}px, ${state}: every control keeps its place`).toEqual(first.controls);
        expect(measured.inOrder, `${width}px, ${state}: controls read in one order`).toBe(true);
        expect(measured.columns, `${width}px, ${state}: one column`).toBeLessThanOrEqual(1);
      }
      expect(states.filter(([, measured]) => measured.columns === 1).length, `${width}px: the list was measured`).toBeGreaterThanOrEqual(3);
      await noHorizontalOverflow(page, `Discover at ${width}px`);
    }
  });

  test("models that share a repository each open their own detail", async ({ page }) => {
    const shared = [{ id: "speech-tiny", name: "Speech Tiny", gib: 0.1 }, { id: "speech-turbo", name: "Speech Turbo", gib: 1.5 }].map((model) => fakeModel({ ...model, task: "transcription", repo: "fixture/speech" }));
    await fakeLibrary(page, () => fakePage(shared));
    await discover(page);
    await page.getByRole("combobox", { name: "Type" }).selectOption({ label: "Speech to text" });
    const rows = page.getByRole("list", { name: "Search results" }).getByRole("listitem");
    await expect(rows).toHaveCount(2);
    const bar = await page.locator(".find-filters").boundingBox();
    await rows.nth(1).getByRole("button", { name: "Speech Turbo", exact: true }).click();
    const detail = page.getByRole("complementary", { name: "Model detail" });
    await expect(detail.getByRole("heading", { name: "Speech Turbo" }), "the second model of the repository, not the first").toBeVisible();
    await expect(detail.getByRole("listbox", { name: "Quants" }).getByRole("option", { selected: true })).toContainText("1.5 GiB");
    expect(await page.locator(".find-filters").boundingBox()).toEqual(bar);
    if (await rows.first().isVisible()) {
      await expect(rows.nth(1)).toHaveClass(/selected/);
      await expect(rows.nth(0), "only the opened model is marked").not.toHaveClass(/selected/);
    }
  });

  test("the detail panel lists every quant with a fit badge", async ({ page }) => {
    await findModels(page, "qwen3 0.6b");
    const qwen = result(page, /^Qwen3 0\.6B$/);
    await qwen.getByRole("button", { name: "11 quants" }).click();
    const detail = page.getByRole("complementary", { name: "Model detail" });
    await expect(detail.getByRole("heading", { name: "Qwen3 0.6B" })).toBeVisible();
    await expect(detail).toContainText("unsloth · apache-2.0");
    const quants = detail.getByRole("listbox", { name: "Quants" });
    await expect(quants.getByRole("option")).toHaveCount(11);
    await expect(quants.getByRole("option", { selected: true }), "the panel opens on the quant the row offers").toContainText("Q8_0");
    await quants.getByRole("option", { name: /Q4_0/ }).click();
    const machine = detail.getByRole("definition").filter({ hasText: /GPU|CPU|too big/ });
    await expect(machine).toHaveText(/^(Fits in GPU memory|Runs on the CPU)$/);
    await expect(detail.getByRole("button", { name: "Download" })).toBeEnabled();
    await quants.getByRole("option", { name: /BF16/ }).click();
    await expect(quants.getByRole("option", { selected: true })).toContainText("BF16");
    await expect(machine).toHaveText("Likely too big for this machine");
    const back = detail.getByRole("button", { name: "Back to results" });
    if (await back.isVisible()) await expect(qwen).toBeHidden();
    else await expect(qwen, "the open model's row is marked").toHaveClass(/selected/);
    await largeText(page);
    await noHorizontalOverflow(page, "Find models detail");
    await (await back.isVisible() ? back : detail.getByRole("button", { name: "Close detail" })).click();
    await expect(detail).toHaveCount(0);
    await expect(qwen).toBeVisible();
  });

  test("picking a quant keeps its row and the results in place", async ({ page }) => {
    let full = 0;
    await page.route("**/v1/juiced/library/search*", async (route) => {
      const hub = !route.request().url().includes("hub=false");
      // A slow refresh leaves time to see a status line or dimming that would move the results.
      if (hub && full > 0) await new Promise((done) => setTimeout(done, 400));
      const response = await route.fetch();
      const body = await response.json();
      if (hub) full++;
      for (const model of body.data) {
        if (model.repo !== "unsloth/Qwen3-0.6B-GGUF") continue;
        const twins = model.quants.map((quant: any) => ({ ...quant, file: `mtp/${quant.file.replace(".gguf", "-MTP.gguf")}`, bytes: quant.bytes + 1, recommended: false, sha256: null, local: { present: false }, jobs: [] }));
        model.quants = [...model.quants, ...twins];
        model.quants[0].jobs = [{ id: "e2e-job", state: "downloading", downloaded_bytes: 1, expected_bytes: model.quants[0].bytes }];
      }
      await route.fulfill({ response, json: body });
    });
    await findModels(page, "qwen3");
    const qwen = result(page, /^Qwen3 0\.6B$/);
    const top = async (locator: Locator) => (await locator.boundingBox())!.y;
    await expect.poll(() => full).toBeGreaterThan(0);
    await expect(page.getByText("Searching Hugging Face…")).toHaveCount(0);
    const seen = new Set<string>(), refreshes = full;
    while (full < refreshes + 2) { const box = (await qwen.boundingBox())!; seen.add(`${box.y},${box.height}`); expect(await page.getByText("Searching Hugging Face…").count()).toBe(0); await page.waitForTimeout(50); }
    expect([...seen], "a refresh moves and resizes no row").toHaveLength(1);
    await qwen.getByRole("button", { name: "22 quants" }).click();
    const quants = page.getByRole("complementary", { name: "Model detail" }).getByRole("listbox", { name: "Quants", exact: true });
    await expect(quants.getByRole("option")).toHaveCount(22);
    for (const index of [15, 9, 20, 14]) {
      const option = quants.getByRole("option").nth(index);
      await option.scrollIntoViewIfNeeded();
      const before = await top(option), height = await quants.evaluate((list) => list.scrollHeight);
      await option.click();
      await expect(option).toHaveAttribute("aria-selected", "true");
      await expect(quants.getByRole("option", { selected: true })).toHaveCount(1);
      expect(await top(option)).toBe(before);
      // Selecting changes no row's height, whatever the font's widths.
      expect(await quants.evaluate((list) => list.scrollHeight)).toBe(height);
    }
    await expect(quants.getByRole("option", { selected: true })).toContainText(/Qwen3-0\.6B-.+/);
  });

  test("a result downloads from its row with progress and is then marked on disk", async ({ page, request }) => {
    const service = api(request);
    const search = await service.json<any>(`/v1/juiced/library/search?q=tiny-chat&limit=12`);
    const sha: string = search.data[0].quants.find((quant: any) => quant.label === "Q4_K_M").sha256;
    await removeDownloaded(service, "Tiny-Chat-Q4_K_M", async () => sha);
    const pulls: number[] = [];
    await page.route("**/v1/juiced/library/pull", async (route) => {
      pulls.push(route.request().postDataJSON().expected_generation);
      if (pulls.length > 1) return route.continue();
      await route.fulfill({ status: 409, contentType: "application/json", body: JSON.stringify({ error: { code: "artifact_revision_conflict", message: "The artifact state changed. Refresh the current revision before retrying." } }) });
    });
    try {
      await discover(page);
      await searchBox(page).fill("tiny-chat");
      const tiny = result(page, /^Tiny-Chat-GGUF$/);
      await expect(tiny.getByText("✓ On disk", { exact: true })).toHaveCount(0);
      const place = async () => { const box = (await tiny.boundingBox())!; return [box.y, box.height]; };
      const before = await place();
      await tiny.getByRole("button", { name: "Download Tiny-Chat-GGUF Q4_K_M" }).click();
      await expect(page.getByRole("status").filter({ hasText: "Downloading Tiny-Chat-GGUF Q4_K_M." })).toBeVisible();
      expect(pulls).toHaveLength(2);
      await expect(page.getByText("The artifact state changed")).toHaveCount(0);
      await expect(tiny.locator(".result-action").getByRole("status")).toHaveText(/^Downloading \d+%$/);
      expect(await place(), "a download in progress keeps the row's place and height").toEqual(before);
      await expect(tiny.getByText("✓ On disk", { exact: true })).toBeVisible({ timeout: 20000 });
      await expect(tiny.locator(".result-quant")).toContainText("Q4_K_M");
      await expect(tiny.getByRole("button", { name: /^Download/ })).toHaveCount(0);
      await expect(tiny.getByRole("button", { name: /^(Open model|View) / })).toBeVisible();
      expect((await service.artifacts()).artifacts.some((item: any) => item.id === sha)).toBe(true);
    } finally { await removeDownloaded(service, "Tiny-Chat-Q4_K_M", async () => sha); }
  });
});

async function view(page: Page, name: "Installed" | "Downloads") {
  await open(page);
  await modelsView(page, name);
}
const table = (page: Page) => page.getByRole("table", { name: "Registered models" });
const fileRow = (page: Page, name: string) => table(page).getByRole("row").filter({ has: page.getByRole("button", { name, exact: true }) });
const fileDetails = (page: Page) => page.getByRole("complementary", { name: "File details" });
async function openFile(page: Page, name: string) {
  const unused = page.getByRole("group", { name: "Status filter" }).getByRole("button", { name: /^Not added \d+$/ });
  if (await fileRow(page, name).count() === 0 && await unused.count()) await unused.click();
  await fileRow(page, name).getByRole("button", { name: `Add ${name}` }).click();
  await expect(fileDetails(page).getByRole("heading", { name, level: 2 })).toBeVisible();
  return fileDetails(page);
}
async function closeFile(page: Page) {
  const back = fileDetails(page).getByRole("button", { name: "Back to models" });
  await (await back.isVisible() ? back : fileDetails(page).getByRole("button", { name: "Close file details" })).click();
  await expect(fileDetails(page)).toHaveCount(0);
}

/** A disposable owned cache repository, so each browser project deletes its own files. */
async function disposableRepo(hub: string, project: string) {
  const repo = `models--e2e--Disposable-${project}-GGUF`;
  const revision = "abcdefabcdefabcdefabcdefabcdefabcdefabcd";
  const snapshot = resolve(hub, repo, "snapshots", revision);
  const blob = resolve(hub, repo, "blobs", "d".repeat(64));
  await mkdir(snapshot, { recursive: true });
  await mkdir(dirname(blob), { recursive: true });
  await writeFile(blob, new Uint8Array(4096));
  await symlink(relative(snapshot, blob), resolve(snapshot, "Disposable-Q4_K_M.gguf")).catch(() => undefined);
  return { name: `Disposable-${project}-GGUF`, file: resolve(snapshot, "Disposable-Q4_K_M.gguf"), blob };
}

test.describe("Installed files", () => {
  test("files no model uses show as Not added, link the models that use them and refuse unsafe deletes", async ({ page, request }, info) => {
    const service = api(request);
    const fixture = await service.source() as any;
    const qwen = `${fixture.cache_hub}/models--unsloth--Qwen3-4B-Instruct-2507-GGUF/snapshots/a6f3b0d5c1e2a4b7c9d8e0f1a2b3c4d5e6f70812/Qwen3-4B-Instruct-2507-Q8_0.gguf`;
    const id = `e2e.cache-q8-${info.project.name}`;
    await service.register({ id, model_file: qwen, command: fixture.command, args: fixture.args });
    await page.route("**/v1/juiced/library/pulls", (route) => route.fulfill({ json: { schema_version: 1, data: [{ pull: "finished", state: "complete", error: null, repo: "unsloth/Qwen3-4B-Instruct-2507-GGUF", label: "Q4_K_M", files: [],
      then: { kind: "add", id: "Qwen3-4B-Instruct-2507-Q4_K_M", policy: "on-demand", state: "failed", error: "artifact_deadline", message: "Artifact verification exceeded its deadline. Preserved files remain available for inspection." } }] } }));
    try {
      await view(page, "Installed");
      await expect(page.getByText(/ GiB on disk/)).toBeVisible();
      const row = fileRow(page, "Qwen3-4B-Instruct-2507-GGUF");
      await expect(row, "unused files stay out of the Enabled list").toHaveCount(0);
      await page.getByRole("button", { name: / GiB not added$/ }).click();
      const status = page.getByRole("group", { name: "Status filter" });
      await expect(status.getByRole("button", { name: /^Not added \d+$/ })).toHaveAttribute("aria-pressed", "true");
      await expect(table(page).getByRole("button", { name: id, exact: true })).toHaveCount(0);
      await expect(row).toContainText("unsloth · Q4_K_M · 2.3 GiB");
      await expect(row).not.toContainText("Q8_0");
      await expect(fileRow(page, "Skyfall-31B-v4.2-GGUF")).toContainText("read-only");
      await page.getByRole("group", { name: "Service filter" }).getByRole("button", { name: /^All \d+$/ }).click();
      await showAllModels(page);
      await expect(table(page).getByRole("button", { name: id, exact: true })).toBeVisible();
      await expect(row).toContainText("Not added");

      const detail = await openFile(page, "Qwen3-4B-Instruct-2507-GGUF");
      const quants = detail.getByRole("listbox", { name: "Quants on disk" });
      await expect(quants.getByRole("option", { selected: true })).toContainText("Q4_K_M");
      await expect(detail.getByRole("button", { name: "Delete Q4_K_M" })).toBeEnabled();
      await expect(detail.getByLabel("Model name")).toHaveValue("Qwen3-4B-Instruct-2507-Q4_K_M");
      await expect(detail.locator(".inspector-title"), "one quant is in use, so the heading doesn't call the repository Not added").not.toContainText("Not added");
      await expect(quants.getByRole("option", { selected: true })).toContainText("Not added");
      await expect(detail.getByText("No model", { exact: true })).toHaveCount(0);
      await expect(detail.getByRole("status").filter({ hasText: "adding it failed" })).toHaveText("The download finished, but adding it failed. Checking the files took too long. Add the model again.");
      await quants.getByRole("option", { name: /Q8_0/ }).click();
      await expect(detail.getByRole("status").filter({ hasText: "adding it failed" })).toHaveCount(0);
      await expect(detail.getByText("Q8_0 used by", { exact: true })).toBeVisible();
      await expect(detail.getByRole("button", { name: "Delete Q8_0" })).toBeDisabled();
      await expect(detail).toContainText(`Used by ${id}. Remove the model first.`);
      await largeText(page);
      await noHorizontalOverflow(page, "File details");
      await page.evaluate(() => { document.documentElement.style.fontSize = ""; });

      await detail.getByRole("button", { name: id, exact: true }).click();
      const inspector = page.getByRole("complementary", { name: "Model inspector" });
      await expect(inspector.getByRole("heading", { name: id, level: 2 })).toBeVisible();
      await inspector.getByRole("tab", { name: "Manage" }).click();
      await expect(inspector.getByRole("list", { name: "Model files" })).toContainText("Qwen3-4B-Instruct-2507-Q8_0.gguf");
      await expect(inspector).toContainText("Its files stay on disk and show as Not added");
      await closeInspector(page);

      const skyfall = await openFile(page, "Skyfall-31B-v4.2-GGUF");
      await expect(skyfall.getByRole("button", { name: "Delete IQ4_NL" })).toBeDisabled();
      await expect(skyfall).toContainText("This folder is mounted read-only.");
      await closeFile(page);
    } finally { await service.remove(id); }
  });

  test("removing a model leads to deleting its unused files after confirmation", async ({ page, request }, info) => {
    const service = api(request);
    const fixture = await service.source() as any;
    const disposable = await disposableRepo(fixture.cache_hub, info.project.name);
    const id = `e2e.disposable-${info.project.name}`;
    await service.register({ id, model_file: disposable.file, command: fixture.command, args: fixture.args });
    try {
      await open(page);
      const inspector = await openModel(page, id);
      await inspector.getByRole("tab", { name: "Manage" }).click();
      await expect(inspector.getByRole("list", { name: "Model files" })).toContainText("Disposable-Q4_K_M.gguf");
      await inspector.getByRole("button", { name: "Remove model", exact: true }).click();
      await page.getByRole("dialog").getByRole("button", { name: "Remove model", exact: true }).click();
      const detail = fileDetails(page);
      await expect(detail.getByRole("heading", { name: disposable.name, level: 2 })).toBeVisible();
      await expect(detail.getByRole("button", { name: "Delete Q4_K_M" })).toBeEnabled();
      await detail.getByRole("button", { name: "Delete Q4_K_M" }).click();
      const dialog = page.getByRole("dialog", { name: `Delete ${disposable.name} Q4_K_M?` });
      await expect(dialog).toContainText("Removes 1 file (4.0 KiB) from disk.");
      await dialog.getByRole("button", { name: "Delete", exact: true }).click();
      await expect(page.getByRole("status").filter({ hasText: `Deleted ${disposable.name} Q4_K_M. Freed 4.0 KiB.` })).toBeVisible();
      await expect(detail).toHaveCount(0);
      await expect(fileRow(page, disposable.name)).toHaveCount(0);
      expect(existsSync(disposable.file)).toBe(false);
      expect(existsSync(disposable.blob)).toBe(false);
    } finally { await service.remove(id); }
  });
});

test.describe("Downloads", () => {
  test("a URL download needs only the link, reads its size, retries, lands unverified in Installed and can be deleted", async ({ page, request }, info) => {
    const service = api(request);
    const fixture = await service.source();
    const name = `weights-${info.project.name}`;
    const url = new URL(fixture.source_url);
    url.pathname = `/unstable/${name}.gguf`;
    await service.removeArtifact(fixture.sha256);
    await open(page);
    await modelsView(page, "Discover");
    await page.getByRole("button", { name: "Add a model file" }).click();
    await page.getByRole("menuitem", { name: "Download from a URL…" }).click();
    const dialog = page.getByRole("dialog", { name: "Download from a URL" });
    await expect(dialog.getByRole("textbox")).toHaveCount(2);
    await expect(dialog.getByRole("button", { name: "Start download" })).toBeDisabled();
    await dialog.getByLabel("Download URL").fill(url.href);
    await dialog.getByLabel("SHA256").fill("not-a-checksum");
    await dialog.getByRole("button", { name: "Start download" }).click();
    await expect(dialog.getByRole("alert")).toContainText("SHA256");
    await dialog.getByLabel("SHA256").fill("");
    const started = page.waitForRequest((outgoing) => outgoing.method() === "POST" && new URL(outgoing.url()).pathname === "/v1/juiced/artifacts/pull");
    await dialog.getByRole("button", { name: "Start download" }).click();
    expect((await started).postDataJSON().download).toEqual({ source_url: url.href });
    await expect(dialog).toHaveCount(0);
    await expect(page.getByRole("status").filter({ hasText: "Download started. Follow it under Downloads." })).toBeVisible();
    await expect(modelsTab(page, "Downloads")).toHaveAttribute("aria-selected", "true");
    const row = page.getByRole("list", { name: "Download list" }).getByRole("listitem", { name: `${name}.gguf` });
    await expect(row).toContainText("unverified");
    await expect(row.getByRole("button", { name: "Retry" })).toBeVisible({ timeout: 15000 });
    await expect(row.getByRole("progressbar", { name: `${name}.gguf progress` })).toBeVisible();
    await expect(row).toContainText(/of 256\.0 KiB/);
    await noHorizontalOverflow(page, "Downloads");
    await row.getByRole("button", { name: "Retry" }).click();
    await expect(row).toHaveCount(0, { timeout: 15000 });
    await expect(modelsTab(page, "Downloads")).toHaveText("Downloads", { timeout: 10000 });

    await modelsTab(page, "Installed").click();
    const detail = await openFile(page, name);
    await expect(detail).toContainText("Juiced storage");
    await expect(detail).toContainText("Unverified");
    await detail.getByRole("button", { name: /^Delete/ }).click();
    await page.getByRole("dialog").getByRole("button", { name: "Delete", exact: true }).click();
    await expect(fileRow(page, name)).toHaveCount(0);
    await expect.poll(async () => (await service.artifacts()).artifacts.some((item: any) => item.id === fixture.sha256), { timeout: 10000 }).toBe(false);
  });

  test("a download whose connection dropped resumes as the same download", async ({ page }) => {
    const pull = { pull: "dropped1", state: "failed", error: "artifact_network", repo: "unsloth/Big-GGUF", label: "Q6_K", revision: "main",
      files: [{ file: "Big-Q6_K.gguf", bytes: 1048576, state: "failed", job: null, downloaded_bytes: 0, source_url: "https://huggingface.co/unsloth/Big-GGUF/resolve/main/Big-Q6_K.gguf", error: "artifact_network" }],
      then: { kind: "add", policy: "on_demand", state: "failed", error: "download_failed", message: "The download didn't finish." } };
    await page.route("**/v1/juiced/library/pulls", (route) => route.fulfill({ json: { schema_version: 1, data: [pull] } }));
    const posts: unknown[] = [];
    await page.route("**/v1/juiced/library/pulls/dropped1", (route) => { posts.push(route.request().postDataJSON()); return route.fulfill({ json: { ...pull, state: "downloading" } }); });
    await view(page, "Downloads");
    const row = page.getByRole("list", { name: "Download list" }).getByRole("listitem").filter({ hasText: "Big-GGUF" });
    await expect(row.getByRole("button", { name: "Resume" })).toBeVisible();
    await row.getByRole("button", { name: "Resume" }).click();
    await expect.poll(() => posts).toEqual([{ action: "resume" }]);
  });

  test("unfinished files are summarised without actions", async ({ page }) => {
    await view(page, "Downloads");
    await expect(page.getByText(/1 unfinished file holds/)).toBeVisible();
    await largeText(page);
    await noHorizontalOverflow(page, "Downloads");
  });
});
