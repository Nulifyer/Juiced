import { api, expect, hotModel, modelId, navigate, open, tab, test, toasts } from "./support";

type AliasList = { revision: number; aliases: { alias: string; target: string }[] };

async function clearAliases(service: ReturnType<typeof api>) {
  for (;;) {
    const list = await service.json<AliasList>("/v1/juiced/aliases");
    const entry = list.aliases.find((item) => item.alias.startsWith("browser-"));
    if (!entry) return;
    await service.post("/v1/juiced/aliases", { alias: entry.alias, target: null, expected_revision: list.revision });
  }
}

test("an alias is created, serves requests, swaps to another model and is removed", async ({ page, request }) => {
  const service = api(request);
  const alias = `browser-default-${test.info().project.name}`;
  await clearAliases(service);
  try {
    await open(page);
    await navigate(page, "Models");
    await tab(page, "Models views", "Aliases").click();
    const view = page.getByRole("region", { name: "Aliases" });
    await expect(view).toContainText("No aliases yet");
    const form = view.getByRole("form", { name: "New alias" });
    await form.getByLabel("Alias name").fill(hotModel);
    await expect(view).toContainText("already a model or an alias");
    await expect(form.getByRole("button", { name: "Add alias" })).toBeDisabled();
    await form.getByLabel("Alias name").fill(alias);
    await form.getByLabel("Alias target").selectOption(hotModel);
    await form.getByRole("button", { name: "Add alias" }).click();
    await expect(toasts(page).getByRole("status")).toContainText(`${alias} now points to ${hotModel}.`);
    await expect(form.getByLabel("Alias name")).toHaveValue("");
    const row = view.getByRole("row", { name: alias });
    await expect(row).toContainText("Serving");

    const models = await service.json<{ data: { id: string; aliased_to?: string }[] }>("/v1/models");
    expect(models.data.find((entry) => entry.id === alias)?.aliased_to).toBe(hotModel);
    const reply = await request.post("/v1/chat/completions", { data: { model: alias, messages: [{ role: "user", content: "hi" }], max_tokens: 4 } });
    expect(reply.ok(), await reply.text()).toBe(true);

    await row.getByLabel(`Target for ${alias}`).selectOption(modelId);
    await expect(toasts(page).getByRole("status")).toContainText(`${alias} now points to ${modelId}.`);
    await expect.poll(async () => (await service.json<AliasList>("/v1/juiced/aliases")).aliases.find((entry) => entry.alias === alias)?.target).toBe(modelId);

    const before = await service.json<AliasList>("/v1/juiced/aliases");
    await service.post("/v1/juiced/aliases", { alias, target: hotModel, expected_revision: before.revision });
    await expect(row.getByLabel(`Target for ${alias}`)).toHaveValue(hotModel, { timeout: 10000 });

    await row.getByRole("button", { name: "Open model" }).click();
    await expect(page.getByRole("complementary", { name: "Model inspector" }).getByRole("heading", { name: hotModel, level: 2 })).toBeVisible();
    await tab(page, "Models views", "Aliases").click();
    await view.getByRole("row", { name: alias }).getByRole("button", { name: `Remove ${alias}` }).click();
    await expect(toasts(page).getByRole("status")).toContainText(`Removed ${alias}.`);
    await expect(view).toContainText("No aliases yet");
    expect((await service.json<AliasList>("/v1/juiced/aliases")).aliases.some((entry) => entry.alias === alias)).toBe(false);
  } finally {
    await clearAliases(service);
  }
});

test("a stale alias revision reports the conflict and keeps the list intact", async ({ page, request }) => {
  const service = api(request);
  const alias = `browser-stale-${test.info().project.name}`;
  await clearAliases(service);
  try {
    await open(page);
    await navigate(page, "Models");
    await tab(page, "Models views", "Aliases").click();
    const view = page.getByRole("region", { name: "Aliases" });
    await page.route("**/v1/juiced/aliases", async (route) => {
      if (route.request().method() !== "POST") return route.fallback();
      const body = route.request().postDataJSON();
      await route.continue({ postData: JSON.stringify({ ...body, expected_revision: body.expected_revision + 1000 }) });
    });
    const form = view.getByRole("form", { name: "New alias" });
    await form.getByLabel("Alias name").fill(alias);
    await form.getByLabel("Alias target").selectOption(hotModel);
    await form.getByRole("button", { name: "Add alias" }).click();
    await expect(view.getByRole("alert")).toContainText("changed in another client");
    await expect(form.getByLabel("Alias name")).toHaveValue(alias);
    await page.unroute("**/v1/juiced/aliases");
    await form.getByRole("button", { name: "Add alias" }).click();
    await expect(view.getByRole("row", { name: alias })).toBeVisible();
  } finally {
    await clearAliases(service);
  }
});
