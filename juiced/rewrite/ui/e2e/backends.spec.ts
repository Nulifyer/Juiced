import { readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import type { Locator, Page } from "@playwright/test";
import { api, expect, navigate, noHorizontalOverflow, open, test } from "./support";

const engines = (page: Page) => page.getByRole("table", { name: "Engines" });
const row = (page: Page, name: string) => engines(page).getByRole("row", { name, exact: true });
const chip = (scope: Locator, text: RegExp) => scope.getByRole("listitem").filter({ hasText: text }).locator(".engine-chip");
const imported = async (page: Page) => Number(/Imported(?: ×(\d+))?/.exec(await row(page, "llama.cpp").innerText())?.[1] ?? 1);

test("one screen shows this machine and the engines it can run, with Show all for the rest", async ({ page }) => {
  await open(page);
  await navigate(page, "Engines & hardware");
  await expect(page.getByRole("main").getByRole("tablist", { name: "Engines & hardware views" }).getByRole("tab")).toHaveText(["Engines", "Storage"]);
  const machine = page.getByRole("region", { name: "This machine" });
  await expect(machine.getByRole("group", { name: "CPU" })).toContainText("Fixture CPU");
  await expect(machine.getByRole("group", { name: "CPU" })).toContainText("4 cores");
  await expect(machine.getByRole("group", { name: "Memory" })).toContainText(/\d+(\.\d)? GiB free/);
  await expect(machine.getByRole("group", { name: "GPU" })).toContainText("None");
  await machine.getByRole("button", { name: "About CPU details" }).click();
  await expect(page.locator(".help-popup:popover-open")).toContainText("avx2");
  await page.keyboard.press("Escape");

  const llama = row(page, "llama.cpp");
  await expect(llama).toContainText("Chat and embeddings");
  await expect(chip(llama, /CPU/).getByLabel("Recommended", { exact: true })).toHaveCount(1);
  await expect(llama.getByLabel("Recommended", { exact: true })).toHaveCount(1);
  await expect(row(page, "whisper.cpp").getByRole("button", { name: /^Install whisper\.cpp CPU, 4\.2 MiB, recommended$/ })).toBeVisible();
  await expect(row(page, "Moonshine").getByRole("button", { name: /Moonshine CPU, 126 MiB/ })).toBeVisible();
  await expect(engines(page)).not.toContainText("0.0 GiB");
  await expect(chip(engines(page), /^CUDA/)).toHaveCount(0);
  await expect(row(page, "FastFlowLM")).toHaveCount(0);
  await expect(page.getByText(/Catalog|Lemonade recipe|Delivery treatment/)).toHaveCount(0);

  const visible = await engines(page).getByRole("row").count();
  const all = page.getByRole("switch", { name: "Show all engines" });
  await all.click();
  await expect(all).toHaveAttribute("aria-checked", "true");
  await expect.poll(() => engines(page).getByRole("row").count()).toBeGreaterThan(visible);
  const npu = chip(row(page, "FastFlowLM"), /^NPU/);
  await expect(npu).toHaveAttribute("title", /No AMD NPU detected/);
  await expect(row(page, "FastFlowLM").getByRole("button")).toHaveCount(0);
  await expect(engines(page).getByRole("row").filter({ hasText: /No installer yet|Not for this machine/ }).first()).toBeVisible();
  await expect(chip(row(page, "llama.cpp"), /^CUDA/)).toHaveAttribute("title", "No NVIDIA GPU detected.");
  await noHorizontalOverflow(page, "Engines & hardware with every engine");
  await all.click();
  await expect(engines(page).getByRole("row")).toHaveCount(visible);
  await expect(page.getByRole("button", { name: /refresh/i })).toHaveCount(0);
});

test("installing a build shows progress, then the installed chip", async ({ page }) => {
  let started = 0;
  const job = { id: "browser-install", engine: "parakeet.cpp", backend: "cpu", package: null as string | null };
  await page.route("**/v1/juiced/engines", async (route) => {
    const document = await (await route.fetch()).json();
    const target = document.installable.find((item: any) => item.engine === "parakeet.cpp" && item.backend === "cpu");
    if (started && Date.now() - started < 2500) document.jobs.push({ ...job, state: "downloading", downloaded_bytes: 1024, total_bytes: 4096 });
    else if (started) {
      target.installed_package = "package-browser-install";
      document.installed.push({ package: "package-browser-install", engine: "parakeet.cpp", backend: null, release: { backend: "cpu" }, models: [{ model: "browser.parakeet" }] });
      document.jobs.push({ ...job, state: "complete", downloaded_bytes: 4096, total_bytes: 4096, package: "package-browser-install" });
    }
    await route.fulfill({ json: document });
  });
  await page.route("**/v1/juiced/engines/install", async (route) => {
    expect(route.request().postDataJSON()).toMatchObject({ engine: "parakeet.cpp", backend: "cpu" });
    started = Date.now();
    await route.fulfill({ status: 202, json: { job: { id: job.id } } });
  });
  await open(page);
  await navigate(page, "Engines & hardware");
  const parakeet = row(page, "Parakeet");
  await parakeet.getByRole("button", { name: /^Install Parakeet CPU, 2\.6 MiB$/ }).click();
  await expect(parakeet.getByRole("progressbar", { name: "Installing Parakeet CPU" })).toBeVisible();
  const installed = parakeet.getByRole("listitem").filter({ hasText: "Installed: CPU" });
  await expect(installed).toContainText("1 model", { timeout: 10000 });
  await expect(installed.locator(".engine-chip")).toHaveAttribute("title", "Installed · used by 1 model");
  await expect(parakeet.getByRole("button")).toHaveCount(0);
  await expect(page.getByRole("alert")).toHaveCount(0);
});

test("a failed install says which build failed and leaves it installable", async ({ page }) => {
  await page.route("**/v1/juiced/engines/install", (route) => route.fulfill({ status: 502, json: { error: { code: "engine_download_failed", message: "The FUTO transcribe CPU download failed. Check the network, then retry." } } }));
  await open(page);
  await navigate(page, "Engines & hardware");
  const futo = row(page, "FUTO transcribe");
  await futo.getByRole("button", { name: /^Install FUTO transcribe CPU/ }).click();
  await expect(page.getByRole("alert")).toContainText("The FUTO transcribe CPU download failed.");
  await expect(futo.getByRole("button", { name: /^Install FUTO transcribe CPU/ })).toBeEnabled();
});

test("a local engine bundle imports through its dialog and shows on its engine", async ({ page, request }) => {
  test.skip(process.platform === "win32", "Managed runtime installation needs the qualified POSIX file owner.");
  const service = api(request);
  const fixture = await service.source();
  const root = dirname(fixture.model_file);
  const inventory = await service.json<any>("/v1/juiced/runtime-packages");
  const source = inventory.packages.find((item: any) => item.provenance.build_revision === "fixture-runtime-first");
  expect(source).toBeTruthy();
  const specification = JSON.parse(await readFile(join(root, "runtime-import.json"), "utf8"));
  const build = `browser-install-${crypto.randomUUID()}`;
  specification.provenance.build_revision = build;
  specification.bundle.root = join(root, "runtime-packages", source.id, "payload");
  const path = join(root, `${build}.json`);
  await writeFile(path, JSON.stringify(specification));
  try {
    await open(page);
    await navigate(page, "Engines & hardware");
    await expect(row(page, "llama.cpp").getByRole("listitem").filter({ hasText: /Installed: Imported/ })).toBeVisible();
    const before = await imported(page);
    const panel = page.getByRole("region", { name: "Local engine bundles" });
    await panel.getByRole("button", { name: "Import a local engine bundle" }).click();
    const dialog = page.getByRole("dialog", { name: "Import engine bundle" });
    const input = dialog.getByLabel("Bundle specification on service host");
    await input.fill("relative-import.json");
    await dialog.getByRole("button", { name: "Import local runtime" }).click();
    await expect(dialog.getByRole("alert")).toContainText("absolute specification-file path");
    await expect(input).toHaveValue("relative-import.json");
    await dialog.getByRole("button", { name: "Clear import draft" }).click();
    await expect(input).toHaveValue("");
    await input.fill(path);
    await dialog.getByRole("button", { name: "Import local runtime" }).click();
    const receipt = dialog.getByRole("status").filter({ hasText: /^Installed: package-/ });
    await expect(receipt).toBeVisible({ timeout: 15000 });
    await expect(input).toHaveValue("");
    await dialog.getByRole("button", { name: "Close dialog" }).click();
    await expect(dialog).toHaveCount(0);
    expect((await service.json<any>("/v1/juiced/runtime-packages")).packages.some((item: any) => item.provenance.build_revision === build)).toBe(true);
    await expect.poll(() => imported(page), { timeout: 10000 }).toBe(before + 1);
  } finally {
    await rm(path, { force: true });
  }
});
