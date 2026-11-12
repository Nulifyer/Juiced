import type { Page } from "@playwright/test";
import { api, expect, largeText, navigate, noHorizontalOverflow, open, tab, test } from "./support";

const tinyRepo = "fixture-labs/Tiny-Chat-GGUF";
const result = (page: Page, name: RegExp) => page.getByRole("list", { name: "Search results" }).getByRole("listitem").filter({ has: page.getByText(name) });

async function findModels(page: Page) {
  await open(page);
  await navigate(page, "Library");
  await expect(tab(page, "Library view", "Find models")).toHaveAttribute("aria-selected", "true");
  await expect(result(page, /^Qwen3 0\.6B$/)).toBeVisible({ timeout: 15000 });
}

test.describe("Find models", () => {
  test("search, task chips and recommended catalog entries", async ({ page }) => {
    await findModels(page);
    const qwen = result(page, /^Qwen3 0\.6B$/);
    await expect(qwen.getByText("Recommended", { exact: true })).toBeVisible();
    await expect(qwen).toContainText("unsloth · chat");
    const chips = qwen.getByRole("group", { name: "Qwen3 0.6B quants" });
    await expect(chips.getByRole("button")).toHaveCount(7);
    await expect(chips.getByRole("button", { name: "+5 more" })).toBeVisible();
    await expect(chips.getByRole("button", { name: /^Q4_0/ }).getByLabel("Suggested")).toBeVisible();
    const tiny = result(page, /^Tiny-Chat-GGUF$/);
    await expect(tiny).toContainText("fixture-labs · chat · 4k downloads");
    await expect(tiny.getByText("Recommended", { exact: true })).toHaveCount(0);
    await expect(tiny.getByRole("group", { name: "Tiny-Chat-GGUF quants" }).getByRole("button")).toHaveCount(2);
    await expect(page.getByRole("button", { name: /refresh/i })).toHaveCount(0);

    await page.getByRole("searchbox", { name: "Search models" }).fill("tiny-chat");
    await expect(page.getByRole("list", { name: "Search results" }).getByRole("listitem")).toHaveCount(1);
    await expect(tiny).toBeVisible();
    await page.getByRole("searchbox", { name: "Search models" }).fill("no-such-model-anywhere");
    await expect(page.getByRole("heading", { name: "No models found" })).toBeVisible();
    await page.getByRole("searchbox", { name: "Search models" }).fill("");

    const task = page.getByRole("group", { name: "Task" });
    await task.getByRole("button", { name: "Embeddings" }).click();
    await expect(task.getByRole("button", { name: "Embeddings" })).toHaveAttribute("aria-pressed", "true");
    await expect(result(page, /^Nomic Embed Text v1$/)).toBeVisible();
    await expect(result(page, /^Qwen3 0\.6B$/)).toHaveCount(0);
    await expect(tiny).toHaveCount(0);
    await task.getByRole("button", { name: "Speech to text" }).click();
    await expect(result(page, /^Whisper Tiny$/)).toBeVisible();
    await expect(result(page, /^Nomic Embed Text v1$/)).toHaveCount(0);
    await task.getByRole("button", { name: "All" }).click();
    await expect(result(page, /^Qwen3 0\.6B$/)).toBeVisible();
  });

  test("the detail panel lists every quant with a fit badge", async ({ page }) => {
    await findModels(page);
    const qwen = result(page, /^Qwen3 0\.6B$/);
    await qwen.getByRole("button", { name: "+5 more" }).click();
    const detail = page.getByRole("complementary", { name: "Model detail" });
    await expect(detail.getByRole("heading", { name: "Qwen3 0.6B" })).toBeVisible();
    await expect(detail).toContainText("unsloth · apache-2.0");
    const quants = detail.getByRole("listbox", { name: "Quants" });
    await expect(quants.getByRole("option")).toHaveCount(11);
    await expect(quants.getByRole("option", { selected: true })).toContainText("Q4_0");
    const machine = detail.getByRole("definition").filter({ hasText: /GPU|CPU|too big/ });
    await expect(machine).toHaveText(/^(Fits in GPU memory|Runs on the CPU)$/);
    await expect(detail.getByRole("button", { name: "Download" })).toBeEnabled();
    await quants.getByRole("option", { name: /BF16/ }).click();
    await expect(quants.getByRole("option", { selected: true })).toContainText("BF16");
    await expect(machine).toHaveText("Likely too big for this machine");
    await expect(qwen.getByRole("group", { name: "Qwen3 0.6B quants" }).getByRole("button", { name: /^BF16/ })).toHaveAttribute("aria-pressed", "true");
    await largeText(page);
    await noHorizontalOverflow(page, "Find models detail");
    await detail.getByRole("button", { name: "Close detail" }).click();
    await expect(detail).toHaveCount(0);
  });

  test("download shows progress and then marks the quant as on this machine", async ({ page, request }) => {
    const service = api(request);
    const search = await service.json<any>(`/v1/juiced/library/search?q=tiny-chat&limit=12`);
    const sha: string = search.data[0].quants.find((quant: any) => quant.label === "Q4_K_M").sha256;
    await service.removeArtifact(sha);
    try {
      await findModels(page);
      const tiny = result(page, /^Tiny-Chat-GGUF$/);
      await expect(tiny.getByText("On this machine", { exact: true })).toHaveCount(0);
      await tiny.getByRole("button", { name: /^Q4_K_M/ }).click();
      const detail = page.getByRole("complementary", { name: "Model detail" });
      await expect(detail.getByRole("heading", { name: "Tiny-Chat-GGUF" })).toBeVisible();
      await detail.getByRole("button", { name: "Download" }).click();
      await expect(page.getByRole("status").filter({ hasText: "Downloading Tiny-Chat-GGUF Q4_K_M." })).toBeVisible();
      await expect(detail.getByRole("progressbar", { name: "Download progress" })).toBeVisible();
      await expect(detail).toContainText(/Downloading… \d+%/);
      await expect(tiny.getByText("On this machine", { exact: true })).toBeVisible({ timeout: 20000 });
      await expect(tiny.getByRole("button", { name: /Q4_K_M/ })).toHaveAccessibleName(/^Downloaded\s*Q4_K_M/);
      await expect(detail.getByRole("progressbar")).toHaveCount(0);
      await expect(detail.getByRole("option", { name: /Q4_K_M/ })).toContainText("✓ downloaded");
      await expect(detail.getByRole("button", { name: "Download" })).toHaveCount(0);
      expect((await service.artifacts()).artifacts.some((item: any) => item.id === sha)).toBe(true);
    } finally {
      await expect.poll(async () => (await service.artifacts()).jobs.every((job: any) => !job.writer_active), { timeout: 20000 }).toBe(true);
      await service.removeArtifact(sha);
    }
  });
});

test.describe("Files on disk", () => {
  async function files(page: Page) {
    await open(page);
    await navigate(page, "Library");
    await tab(page, "Library view", "Files on disk").click();
    await expect(page.getByRole("region", { name: "Available weight files" })).toBeVisible();
  }

  test("Add from URL downloads with a checksum and Remove weights deletes the file", async ({ page, request }) => {
    const service = api(request);
    const fixture = await service.source();
    await service.removeArtifact(fixture.sha256);
    await files(page);
    await page.getByRole("button", { name: "Add from URL" }).click();
    const dialog = page.getByRole("dialog", { name: "Add weights from a URL" });
    await dialog.getByLabel("Download URL").fill(fixture.source_url);
    await dialog.getByLabel("Publisher SHA256").fill("not-a-checksum");
    await dialog.getByLabel("Storage ceiling in bytes").fill(String(fixture.bytes));
    await dialog.getByRole("button", { name: "Start download" }).click();
    await expect(dialog.getByRole("alert")).toContainText("SHA256");
    await expect(dialog.getByLabel("Download URL")).toHaveValue(fixture.source_url);
    await dialog.getByLabel("Publisher SHA256").fill(fixture.sha256);
    await dialog.getByLabel("Expected size in bytes").fill(String(fixture.bytes));
    await dialog.getByRole("button", { name: "Start download" }).click();
    await expect(dialog).toHaveCount(0);
    await expect(page.getByRole("status").filter({ hasText: "Download started." })).toBeVisible();
    const jobs = page.getByRole("region", { name: "Download jobs" });
    const job = jobs.getByRole("article").filter({ hasText: "weights.gguf" }).first();
    await expect(job).toBeVisible();
    await expect(job.getByRole("progressbar")).toBeVisible();
    await expect(job).toContainText("complete", { timeout: 15000 });
    const file = page.getByRole("article", { name: `Weight file ${fixture.sha256}`, exact: true });
    await expect(file).toContainText("Verified");
    await expect(file).toContainText("256.0 KiB");
    await job.getByRole("button", { name: "Dismiss job" }).click();
    await expect(page.getByRole("status").filter({ hasText: "Job dismissed. Weights kept." })).toBeVisible();
    await expect(file).toBeVisible();
    await file.getByRole("button", { name: "Remove weights" }).click();
    await expect(page.getByRole("status").filter({ hasText: "Weights removed." })).toBeVisible();
    await expect(file).toHaveCount(0);
    expect((await service.artifacts()).artifacts.some((item: any) => item.id === fixture.sha256)).toBe(false);
  });

  test("model sets are created from a weight file and removed without deleting it", async ({ page, request }) => {
    const service = api(request);
    const fixture = await service.source();
    await service.acquire(fixture);
    const created = new Set<string>();
    try {
      await files(page);
      const file = page.getByRole("article", { name: `Weight file ${fixture.sha256}`, exact: true });
      await file.getByRole("button", { name: "Create model set" }).click();
      await expect(page.getByRole("status").filter({ hasText: "Model set created." })).toBeVisible();
      const form = page.getByRole("form", { name: "Register managed model" });
      await expect(form).toContainText("Artifact set: set-");
      await form.getByRole("button", { name: "Close registration" }).click();
      const inventory = await service.artifacts();
      const record = inventory.artifact_sets.find((set: any) => set.specification.origin.importer_id === "juiced-console" && set.specification.members[0].artifact === fixture.sha256);
      created.add(record.id);
      const sets = page.getByRole("region", { name: "Artifact sets" });
      const row = sets.getByRole("article", { name: `Artifact set ${record.id}`, exact: true });
      await expect(row).toContainText("Primary file");
      await expect(row).toContainText("juiced-console");
      await expect(file.getByRole("button", { name: "Remove weights" })).toBeDisabled();
      await row.getByRole("button", { name: "Remove set" }).click();
      await expect(page.getByRole("status").filter({ hasText: "Set removed. Its files are kept." })).toBeVisible();
      await expect(row).toHaveCount(0);
      created.delete(record.id);
      await expect(file).toBeVisible();
      await expect(file.getByRole("button", { name: "Remove weights" })).toBeEnabled();
    } finally {
      for (const set of created) await service.removeSet(set);
      await service.removeArtifact(fixture.sha256);
    }
  });

  test("many model sets page through the inventory", async ({ page, request }) => {
    const service = api(request);
    const fixture = await service.source();
    await service.acquire(fixture);
    const created: string[] = [];
    try {
      for (let index = 0; index < 65; index++) created.push((await service.createSet(fixture.sha256, fixture.bytes, "browser-paging")).id);
      await files(page);
      const sets = page.getByRole("region", { name: "Artifact sets" });
      const pager = page.getByLabel("Set pages");
      await expect(pager).toContainText("Page 1 · 65 total");
      await expect(sets.getByRole("article")).toHaveCount(64);
      await expect(pager.getByRole("button", { name: "Previous set page" })).toBeDisabled();
      await pager.getByRole("button", { name: "Next set page" }).click();
      await expect(pager).toContainText("Page 2 · 65 total");
      await expect(sets.getByRole("article")).toHaveCount(1);
      await expect(pager.getByRole("button", { name: "Next set page" })).toBeDisabled();
      await pager.getByRole("button", { name: "Previous set page" }).click();
      await expect(pager).toContainText("Page 1");
      await expect(sets.getByRole("article")).toHaveCount(64);
    } finally {
      for (const set of created) await service.removeSet(set);
      await service.removeArtifact(fixture.sha256);
    }
    await expect(page.getByLabel("Set pages")).toHaveCount(0, { timeout: 10000 });
  });

  test("unclaimed retained files are listed without actions", async ({ page, request }) => {
    const filename = `job-${"e".repeat(32)}.part`;
    const inventory = await api(request).artifacts();
    expect(inventory.quarantined_files.some((file: { filename: string }) => file.filename === filename)).toBe(true);
    await files(page);
    const retained = page.getByRole("region", { name: "Retained files awaiting inspection" });
    await expect(retained.getByText(filename, { exact: true })).toBeVisible();
    await expect(retained.getByRole("heading", { name: "Unclaimed files" })).toBeVisible();
    await expect(retained.getByRole("button", { name: /^(?!About)/ })).toHaveCount(0);
    await largeText(page);
    await noHorizontalOverflow(page, "Files on disk");
  });
});
