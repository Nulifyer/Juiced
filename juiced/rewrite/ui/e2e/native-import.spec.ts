import { api, expect, navigate, open, tab, test, toasts } from "./support";

test("settings from an old Juiced folder are previewed and imported", async ({ page, request }) => {
  const service = api(request);
  const fixture = await service.source();
  const source = `${fixture.import_root}/old-juiced/config`;
  type Preview = { models: { native_id: string; proposed_id: string }[] };
  const proposed = (await service.post<Preview>("/v1/juiced/import/native", { action: "preview", source })).models.find((row) => row.native_id === "user.OldChat")!.proposed_id;
  await service.remove(proposed);
  try {
    await open(page);
    await navigate(page, "Library");
    await tab(page, "Library view", "Import").click();
    const view = page.getByRole("region", { name: "Import from Juiced 0.9" });
    const folders = view.getByRole("list", { name: "Folders" });
    await folders.getByRole("button").first().click();
    await folders.getByRole("button", { name: "old-juiced" }).click();
    await folders.getByRole("button", { name: "config" }).click();
    await view.getByRole("button", { name: "Use this folder" }).click();
    const row = view.getByRole("table", { name: "Models to import" }).getByRole("row", { name: "user.OldChat" });
    await expect(row).toContainText(proposed);
    await expect(row.getByRole("option", { name: "On demand (before)" })).toBeAttached();
    const install = row.getByRole("button", { name: "Install" });
    if (await install.isVisible()) { await install.click(); await expect(row).toContainText("Ready", { timeout: 30000 }); }
    await expect(row.getByLabel("Import user.OldChat")).toBeChecked();
    await expect(row.getByLabel("Policy for user.OldChat")).toHaveValue("disabled");
    await row.getByLabel("Policy for user.OldChat").selectOption("on-demand");
    await view.getByRole("button", { name: /^Import \d+ models?$/ }).click();
    await expect(toasts(page).getByRole("status")).toContainText("Imported 1 model.");
    await expect(row).toContainText("Already added");
    expect((await service.model(proposed))?.policy).toBe("on-demand");
  } finally {
    await service.remove(proposed);
  }
});
