import type { Page } from "@playwright/test";
import { existsSync } from "node:fs";
import { mkdir, symlink, writeFile } from "node:fs/promises";
import { dirname, relative, resolve } from "node:path";
import { api, closeInspector, expect, largeText, modelsTab, modelsView, noHorizontalOverflow, open, openModel, test } from "./support";

const tinyRepo = "fixture-labs/Tiny-Chat-GGUF";
const result = (page: Page, name: RegExp) => page.getByRole("list", { name: "Search results" }).getByRole("listitem").filter({ has: page.getByText(name) });

async function findModels(page: Page) {
  await open(page);
  await modelsView(page, "Discover");
  await expect(result(page, /^Qwen3 0\.6B$/)).toBeVisible({ timeout: 15000 });
}

test.describe("Discover", () => {
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
    const back = detail.getByRole("button", { name: "Back to results" });
    if (await back.isVisible()) await expect(qwen).toBeHidden();
    else await expect(qwen.getByRole("group", { name: "Qwen3 0.6B quants" }).getByRole("button", { name: /^BF16/ })).toHaveAttribute("aria-pressed", "true");
    await largeText(page);
    await noHorizontalOverflow(page, "Find models detail");
    await (await back.isVisible() ? back : detail.getByRole("button", { name: "Close detail" })).click();
    await expect(detail).toHaveCount(0);
    await expect(qwen).toBeVisible();
  });

  test("download shows progress and then marks the quant as on this machine", async ({ page, request }) => {
    const service = api(request);
    const search = await service.json<any>(`/v1/juiced/library/search?q=tiny-chat&limit=12`);
    const sha: string = search.data[0].quants.find((quant: any) => quant.label === "Q4_K_M").sha256;
    await service.removeArtifact(sha);
    try {
      await findModels(page);
      const tiny = result(page, /^Tiny-Chat-GGUF$/);
      await expect(tiny.getByText("✓ On this machine", { exact: true })).toHaveCount(0);
      await tiny.getByRole("button", { name: /^Q4_K_M/ }).click();
      const detail = page.getByRole("complementary", { name: "Model detail" });
      await expect(detail.getByRole("heading", { name: "Tiny-Chat-GGUF" })).toBeVisible();
      await detail.getByRole("button", { name: "Download" }).click();
      await expect(page.getByRole("status").filter({ hasText: "Downloading Tiny-Chat-GGUF Q4_K_M." })).toBeVisible();
      await expect(detail.getByRole("progressbar", { name: "Download progress" })).toBeVisible();
      await expect(detail).toContainText(/Downloading… \d+%/);
      await expect(detail.getByRole("option", { name: /Q4_K_M/ })).toContainText("✓ downloaded", { timeout: 20000 });
      await expect(detail.getByRole("progressbar")).toHaveCount(0);
      if (await detail.getByRole("button", { name: "Back to results" }).isVisible()) await detail.getByRole("button", { name: "Back to results" }).click();
      await expect(tiny.getByText("✓ On this machine", { exact: true })).toBeVisible();
      await expect(tiny.getByRole("button", { name: /Q4_K_M/ })).toHaveAccessibleName(/^Downloaded\s*Q4_K_M/);
      if (await detail.count() === 0) await tiny.getByRole("button", { name: /Q4_K_M/ }).click();
      await expect(detail.getByRole("button", { name: "Download" })).toHaveCount(0);
      expect((await service.artifacts()).artifacts.some((item: any) => item.id === sha)).toBe(true);
    } finally {
      await expect.poll(async () => (await service.artifacts()).jobs.every((job: any) => !job.writer_active), { timeout: 20000 }).toBe(true);
      await service.removeArtifact(sha);
    }
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
    try {
      await view(page, "Installed");
      await expect(page.getByText(/ GiB on disk/)).toBeVisible();
      const row = fileRow(page, "Qwen3-4B-Instruct-2507-GGUF");
      await expect(row).toContainText("Not added");
      await expect(row).toContainText("unsloth · Q4_K_M · 2.3 GiB");
      await expect(row).not.toContainText("Q8_0");
      await expect(fileRow(page, "Skyfall-31B-v4.2-GGUF")).toContainText("read-only");

      const filter = page.getByRole("group", { name: "Service filter" });
      await filter.getByRole("button", { name: /^Not added \d+$/ }).click();
      await expect(table(page).getByRole("button", { name: id, exact: true })).toHaveCount(0);
      await expect(row).toBeVisible();
      await filter.getByRole("button", { name: /^All \d+$/ }).click();
      await expect(table(page).getByRole("button", { name: id, exact: true })).toBeVisible();

      const detail = await openFile(page, "Qwen3-4B-Instruct-2507-GGUF");
      const quants = detail.getByRole("listbox", { name: "Quants on disk" });
      await expect(quants.getByRole("option", { selected: true })).toContainText("Q4_K_M");
      await expect(detail.getByRole("button", { name: "Delete Q4_K_M" })).toBeEnabled();
      await expect(detail.getByLabel("Model name")).toHaveValue("Qwen3-4B-Instruct-2507-Q4_K_M");
      await quants.getByRole("option", { name: /Q8_0/ }).click();
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
      await inspector.getByRole("button", { name: "Remove registration" }).click();
      const detail = fileDetails(page);
      await expect(detail.getByRole("heading", { name: disposable.name, level: 2 })).toBeVisible();
      await expect(detail.getByRole("button", { name: "Delete Q4_K_M" })).toBeEnabled();
      await detail.getByRole("button", { name: "Delete Q4_K_M" }).click();
      const dialog = page.getByRole("dialog", { name: `Delete ${disposable.name} Q4_K_M?` });
      await expect(dialog).toContainText("This removes 1 file (4.0 KiB) from disk.");
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
  test("a URL download shows progress, retries, lands in Installed and can be deleted", async ({ page, request }, info) => {
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
    await dialog.getByLabel("Download URL").fill(url.href);
    await dialog.getByLabel("Publisher SHA256").fill("not-a-checksum");
    await dialog.getByLabel("Storage ceiling in bytes").fill(String(fixture.bytes));
    await dialog.getByRole("button", { name: "Start download" }).click();
    await expect(dialog.getByRole("alert")).toContainText("SHA256");
    await dialog.getByLabel("Publisher SHA256").fill(fixture.sha256);
    await dialog.getByLabel("Expected size in bytes").fill(String(fixture.bytes));
    await dialog.getByRole("button", { name: "Start download" }).click();
    await expect(dialog).toHaveCount(0);
    await expect(page.getByRole("status").filter({ hasText: "Download started. Follow it under Downloads." })).toBeVisible();
    await expect(modelsTab(page, "Downloads")).toHaveAttribute("aria-selected", "true");
    const row = page.getByRole("list", { name: "Download list" }).getByRole("listitem", { name: `${name}.gguf` });
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
    await detail.getByRole("button", { name: /^Delete/ }).click();
    await page.getByRole("dialog").getByRole("button", { name: "Delete", exact: true }).click();
    await expect(fileRow(page, name)).toHaveCount(0);
    await expect.poll(async () => (await service.artifacts()).artifacts.some((item: any) => item.id === fixture.sha256), { timeout: 10000 }).toBe(false);
  });

  test("unfinished files are summarised without actions", async ({ page }) => {
    await view(page, "Downloads");
    await expect(page.getByText(/1 unfinished file holds/)).toBeVisible();
    await largeText(page);
    await noHorizontalOverflow(page, "Downloads");
  });
});
