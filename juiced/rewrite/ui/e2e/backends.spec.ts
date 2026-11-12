import { readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { api, expect, navigate, open, tab, test } from "./support";

test("This machine shows device cards and filters available backends", async ({ page }) => {
  await open(page);
  await navigate(page, "Backends & hardware");
  await expect(page.getByRole("tablist", { name: "Backend views" }).getByRole("tab")).toHaveText(["This machine", "Engines", "Catalog"]);
  await expect(tab(page, "Backend views", "This machine")).toHaveAttribute("aria-selected", "true");
  const devices = page.getByRole("region", { name: "Detected hardware" });
  await expect(devices.getByRole("article").first()).toContainText("CPU");
  await expect(devices.getByRole("article").first()).toContainText(/\d+ cores · \d+ threads/);
  await expect(devices.getByRole("article").filter({ hasText: "Memory" })).toContainText(/\d+(\.\d)? GiB free/);
  await expect(devices.getByRole("article").filter({ hasText: /GPU/ }).first()).toBeVisible();
  const filter = page.getByRole("group", { name: "Backend filter" });
  await expect(filter.getByRole("button", { name: "Available" })).toHaveAttribute("aria-pressed", "true");
  const rows = page.getByRole("table", { name: "Backends" }).getByRole("row");
  await expect(rows.first()).toBeVisible();
  const available = await rows.count();
  await filter.getByRole("button", { name: "All" }).click();
  await expect(filter.getByRole("button", { name: "All" })).toHaveAttribute("aria-pressed", "true");
  await expect.poll(() => rows.count()).toBeGreaterThan(available);
  await filter.getByRole("button", { name: "Available" }).click();
  await expect(rows).toHaveCount(available);
  await expect(page.getByRole("button", { name: /refresh/i })).toHaveCount(0);
});

test("the Engines tab lists engines for this host and imports a local bundle through its dialog", async ({ page, request }) => {
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
    await navigate(page, "Backends & hardware");
    await tab(page, "Backend views", "Engines").click();
    await expect(page.getByRole("table", { name: "Engines" }).getByRole("row").nth(1)).toBeVisible();
    const panel = page.getByRole("region", { name: "Installed runtime packages" });
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
  } finally {
    await rm(path, { force: true });
  }
});

test("the Catalog tab resolves a native recipe and searches definitions", async ({ page }) => {
  await open(page);
  await navigate(page, "Backends & hardware");
  await tab(page, "Backend views", "Catalog").click();
  const catalog = page.getByRole("region", { name: "Runtime catalog" });
  const resolver = catalog.getByRole("region", { name: "Resolve a native recipe" });
  await expect(resolver.getByLabel("Native recipe")).toHaveValue("llamacpp");
  await expect(resolver.getByLabel("Native backend label")).toHaveValue("cpu");
  await resolver.getByRole("button", { name: "Resolve mapping" }).click();
  const resolution = resolver.getByRole("region", { name: "Native recipe resolution" });
  await expect(resolution.getByRole("heading", { name: "llama-cpp" })).toBeVisible();
  await expect(resolution).toContainText("Not executable");
  await expect(resolution).toContainText("llamacpp / cpu");
  await resolver.getByLabel("Native backend label").selectOption("hybrid-rocm");
  await expect(resolution).toHaveCount(0);
  await resolver.getByRole("button", { name: "Resolve mapping" }).click();
  await expect(resolution.getByRole("heading", { name: "llama-xdna-hybrid" })).toBeVisible();
  const search = catalog.getByLabel("Search runtime definitions");
  await search.fill("transcribe");
  await expect(catalog.getByRole("article")).toHaveCount(1);
  await expect(catalog.getByRole("article").getByRole("link").first()).toBeVisible();
  await expect(catalog.getByRole("article").getByRole("button")).toHaveCount(0);
  await search.fill("no-such-runtime-definition");
  await expect(catalog.getByRole("heading", { name: "No matches" })).toBeVisible();
});
