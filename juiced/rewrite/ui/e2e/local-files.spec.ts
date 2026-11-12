import type { Locator, Page } from "@playwright/test";
import { api, expect, modelsView, open, test } from "./support";

async function addFrom(detail: Locator, id: string) {
  await detail.getByLabel("Model name").fill(id);
  await expect(detail.getByRole("radio", { name: "On demand" })).toHaveAttribute("aria-checked", "true");
  await detail.getByRole("button", { name: /^(Add model|Install engine and add)$/ }).click();
  const install = detail.getByRole("button", { name: "Install engine and add" });
  await expect(detail.getByRole("status").or(install)).toBeVisible({ timeout: 15000 });
  if (await install.isVisible()) await install.click();
  await expect(detail.getByRole("status")).toContainText(`Added ${id}.`, { timeout: 30000 });
}

async function openMenu(page: Page) {
  await open(page);
  await modelsView(page, "Discover");
  await page.getByRole("button", { name: "Add a model file" }).click();
}

test("a model file uploaded from this computer is added with the same add step", async ({ page, request }) => {
  const service = api(request);
  const id = `browser-upload-${test.info().project.name}`;
  const fixture = await service.source();
  await service.remove(id);
  try {
    await openMenu(page);
    const chooser = page.waitForEvent("filechooser");
    await page.getByRole("menuitem", { name: "Upload from this computer…" }).click();
    await (await chooser).setFiles(fixture.upload_file);
    const detail = page.getByRole("complementary", { name: "Model detail" });
    await expect(detail.getByRole("heading", { name: "Upload-Sample-Q8_0.gguf" })).toBeVisible();
    await expect(detail).toContainText("Uploaded to library");
    await expect(detail.getByLabel("Model name")).toHaveValue("Upload-Sample-Q8_0");
    await addFrom(detail, id);
    const model = (await service.json<{ data: { id: string; policy: string }[] }>("/v1/juiced/catalog")).data.find((entry) => entry.id === id);
    expect(model?.policy).toBe("on-demand");
    await detail.getByRole("button", { name: /^(Back to results|Close detail)$/ }).filter({ visible: true }).first().click();
    await expect(detail).toBeHidden();
  } finally {
    await service.remove(id);
  }
});

test("a file in the server import folder is added in place", async ({ page, request }) => {
  const service = api(request);
  const id = `browser-import-${test.info().project.name}`;
  await service.remove(id);
  try {
    await openMenu(page);
    await page.getByRole("menuitem", { name: "Pick from the server…" }).click();
    const dialog = page.getByRole("dialog", { name: "Add from the server" });
    const files = dialog.getByRole("list", { name: "Server files" });
    await files.getByRole("button").first().click();
    await files.getByRole("button", { name: "nested" }).click();
    await expect(dialog.getByRole("radio", { name: "Where it is" })).toHaveAttribute("aria-checked", "true");
    await files.getByRole("button", { name: /Local-Import-Q8_0\.gguf/ }).click();
    await expect(dialog).toBeHidden();
    const detail = page.getByRole("complementary", { name: "Model detail" });
    await expect(detail.getByRole("heading", { name: "Local-Import-Q8_0.gguf" })).toBeVisible();
    await expect(detail).toContainText("Used where it is");
    await addFrom(detail, id);
    const model = (await service.json<{ data: { id: string; policy: string }[] }>("/v1/juiced/catalog")).data.find((entry) => entry.id === id);
    expect(model?.policy).toBe("on-demand");
  } finally {
    await service.remove(id);
  }
});
