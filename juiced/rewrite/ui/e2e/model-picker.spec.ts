import { api, expect, hotModel, modelId, navigate, open, pickerPopup, tab, test } from "./support";

const fill = Array.from({ length: 14 }, (_, index) => `browser-picker-${String(index).padStart(2, "0")}`);

test("the model picker filters, groups, selects by keyboard and restores on Escape", async ({ page, request }) => {
  const service = api(request);
  await service.policy(modelId, "on-demand");
  const fixture = await service.source();
  for (const id of fill) await service.register({ id, model_file: fixture.model_file, command: fixture.command, args: fixture.args });
  try {
    await open(page);
    await navigate(page, "Models");
    await tab(page, "Models views", "Aliases").click();
    const picker = page.getByRole("form", { name: "New alias" }).getByLabel("Alias target");
    const popup = pickerPopup(picker);
    const search = popup.getByRole("combobox", { name: "Search" });
    const options = popup.getByRole("option");
    await expect(picker).toHaveText("Choose a model…");
    await expect(picker).toHaveAttribute("aria-expanded", "false");

    await picker.focus();
    await page.keyboard.press("ArrowDown");
    await expect(picker).toHaveAttribute("aria-expanded", "true");
    await expect(popup.getByRole("listbox").getByRole("group").first()).toHaveAccessibleName(/^Loaded/);
    await expect(popup.getByRole("group", { name: /^Loaded/ }).getByRole("option").first()).toContainText(hotModel);
    await expect(popup.locator(`[data-value="${fill[0]}"]`), "Disabled models wait behind one row").toHaveCount(0);
    const total = (await service.catalog()).length;
    const disabled = (await service.catalog()).filter((model) => model.policy === "disabled" && !model.loaded).length;
    await expect(popup.getByRole("status")).toHaveText(`${total - disabled} of ${total}`);
    await expect(options.last()).toHaveText(`Show ${disabled} disabled`);

    await page.keyboard.press("End");
    await page.keyboard.press("Enter");
    await expect(popup.getByRole("group", { name: /^Disabled/ }).getByRole("option")).toHaveCount(disabled);
    await expect(popup.getByRole("status")).toHaveText(`${total} of ${total}`);
    await expect(picker, "showing Disabled models keeps the list open").toHaveAttribute("aria-expanded", "true");

    await search.fill("picker-1");
    await expect(options).toHaveText(fill.slice(10).map((id) => new RegExp(`^${id}`)));
    await expect(popup.getByRole("status")).toHaveText(`4 of ${total}`);
    await page.keyboard.press("ArrowDown");
    await page.keyboard.press("ArrowDown");
    await expect(search).toHaveAttribute("aria-activedescendant", await options.nth(2).getAttribute("id") as string);
    await page.keyboard.press("Home");
    await expect(search, "with text typed, Home moves the caret, not the list").toHaveAttribute("aria-activedescendant", await options.nth(2).getAttribute("id") as string);
    await page.keyboard.press("PageDown");
    await expect(search).toHaveAttribute("aria-activedescendant", await options.nth(3).getAttribute("id") as string);
    await page.keyboard.press("ArrowUp");

    await page.keyboard.press("Escape");
    await expect(popup).toHaveCount(0);
    await expect(picker, "Escape keeps the earlier choice").toHaveText("Choose a model…");
    await expect(picker).toBeFocused();

    await page.keyboard.press("p");
    await expect(search).toHaveValue("p");
    await search.fill("no-such-model");
    await expect(popup).toContainText('No model matches "no-such-model".');
    await expect(options).toHaveCount(0);
    await search.fill("PICKER 12");
    await expect(options).toHaveCount(1);
    await page.keyboard.press("Enter");
    await expect(popup).toHaveCount(0);
    await expect(picker).toHaveText(fill[12]!);
    await expect(picker).toBeFocused();

    await picker.click();
    await expect(popup.getByRole("option", { selected: true }), "a selected Disabled model stays listed").toContainText(fill[12]!);
    await expect(options.last()).toHaveText(`Show ${disabled - 1} disabled`);
    const box = await popup.boundingBox(), view = page.viewportSize()!;
    expect(box!.x).toBeGreaterThanOrEqual(0);
    expect(box!.x + box!.width).toBeLessThanOrEqual(view.width);
    expect(box!.y + box!.height).toBeLessThanOrEqual(view.height);
    const row = await options.first().boundingBox();
    expect(row!.height).toBeGreaterThanOrEqual(test.info().project.name === "phone" ? 44 : 24);
    await picker.click();
    await expect(popup).toHaveCount(0);
  } finally {
    for (const id of fill) await service.remove(id);
  }
});
