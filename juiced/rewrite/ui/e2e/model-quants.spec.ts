import { api, expect, noHorizontalOverflow, open, openModel, test } from "./support";

const snapshots = { "Qwen3-4B-Instruct-2507": "a6f3b0d5c1e2a4b7c9d8e0f1a2b3c4d5e6f70812" };

test.describe("Model quants", () => {
  test("a model lists its other quants on disk and opens the one that isn't added", async ({ page, request }, info) => {
    const service = api(request);
    const fixture = await service.source() as any;
    const id = `e2e.quants-4b-${info.project.name}`;
    await service.register({ id, model_file: `${fixture.cache_hub}/models--unsloth--Qwen3-4B-Instruct-2507-GGUF/snapshots/${snapshots["Qwen3-4B-Instruct-2507"]}/Qwen3-4B-Instruct-2507-Q8_0.gguf`, command: fixture.command, args: fixture.args });
    try {
      await open(page);
      const inspector = await openModel(page, id);
      const quants = inspector.getByRole("region", { name: "Quants", exact: true }).getByRole("list", { name: "Quants of Qwen3-4B-Instruct-2507-GGUF" });
      await expect(quants.getByRole("listitem")).toHaveText([/^Q4_K_MNot added.*GiBAdd$/, /^Q8_0This model.*GiB$/]);
      await noHorizontalOverflow(page, "Model quants");
      await quants.getByRole("button", { name: "Add Q4_K_M" }).click();
      const file = page.getByRole("complementary", { name: "File details" });
      await expect(file.getByRole("listbox", { name: "Quants on disk" }).getByRole("option")).toHaveText([/^Q4_K_MNot added.*GiB$/, /^Q8_0Disabled.*GiB$/]);
      await expect(file.getByRole("listbox", { name: "Quants on disk" }).getByRole("option", { selected: true })).toContainText("Q4_K_M");
      await file.getByRole("button", { name: "More quants in Discover" }).click();
      await expect(page.getByRole("searchbox", { name: "Search models" })).toHaveValue("unsloth/Qwen3-4B-Instruct-2507-GGUF");
    } finally { await service.remove(id); }
  });

  test("a quant that can be downloaded opens in Discover with the policy choice", async ({ page, request }, info) => {
    const service = api(request);
    const fixture = await service.source() as any;
    const local = await service.json<any>("/v1/juiced/library/local?fresh=true");
    const file: string = local.groups.find((group: any) => group.repo === "unsloth/Qwen3-0.6B-GGUF").quants.find((quant: any) => quant.label === "Q8_0").file;
    const id = `e2e.quants-06b-${info.project.name}`;
    await service.register({ id, model_file: file, command: fixture.command, args: fixture.args });
    try {
      await open(page);
      const inspector = await openModel(page, id);
      const section = inspector.getByRole("region", { name: "Quants", exact: true });
      const quants = section.getByRole("list", { name: "Quants of Qwen3-0.6B-GGUF" });
      await expect(quants.getByRole("listitem").filter({ hasText: "This model" })).toContainText("Q8_0");
      await expect(quants.getByRole("button", { name: /^Get / })).toHaveCount(6);
      await expect(section.getByRole("button", { name: "+4 more in Discover" })).toBeVisible();
      await expect(section.getByText("Checking Hugging Face…")).toHaveCount(0);
      await noHorizontalOverflow(page, "Model quants with downloads");
      await quants.getByRole("button", { name: "Get Q4_K_M" }).click();
      const detail = page.getByRole("complementary", { name: "Model detail" });
      await expect(detail.getByRole("listbox", { name: "Quants", exact: true }).getByRole("option", { selected: true })).toContainText("Q4_K_M");
      await expect(detail.getByRole("listbox", { name: "Quants", exact: true }).getByRole("option", { name: /Q8_0/ }), "Discover names the policy of the model that runs a quant").toContainText("Disabled");
      await expect(detail.getByRole("radiogroup", { name: "Loading policy" })).toBeVisible();
      await expect(detail.getByRole("button", { name: "Download" })).toBeEnabled();
    } finally { await service.remove(id); }
  });

  test("a model that isn't from Hugging Face says so and lists no downloads", async ({ page }) => {
    let searched = false;
    await page.route("**/v1/juiced/library/search*", async (route) => { searched = true; await route.continue(); });
    await open(page);
    const inspector = await openModel(page, "fixture.chat-small-q4");
    const section = inspector.getByRole("region", { name: "Quants", exact: true });
    await expect(section).toContainText("Not from Hugging Face, so no downloads are listed.");
    await expect(section.getByRole("button", { name: /^Get / })).toHaveCount(0);
    expect(searched).toBe(false);
  });

  test("an unreachable Hugging Face keeps the quants on disk and offers a retry", async ({ page, request }, info) => {
    const service = api(request);
    const fixture = await service.source() as any;
    const id = `e2e.quants-offline-${info.project.name}`;
    await service.register({ id, model_file: `${fixture.cache_hub}/models--unsloth--Qwen3-4B-Instruct-2507-GGUF/snapshots/${snapshots["Qwen3-4B-Instruct-2507"]}/Qwen3-4B-Instruct-2507-Q8_0.gguf`, command: fixture.command, args: fixture.args });
    let reachable = false;
    await page.route("**/v1/juiced/library/search*", async (route) => {
      const response = await route.fetch();
      const body = await response.json();
      if (!reachable) body.sources.huggingface.status = "unavailable";
      await route.fulfill({ response, json: body });
    });
    try {
      await open(page);
      const inspector = await openModel(page, id);
      const section = inspector.getByRole("region", { name: "Quants", exact: true });
      await expect(section).toContainText("Hugging Face is unreachable. Only quants on this machine are listed.");
      await expect(section.getByRole("listitem")).toHaveCount(2);
      reachable = true;
      await section.getByRole("button", { name: "Retry" }).click();
      await expect(section).not.toContainText("unreachable");
      await expect(section.getByRole("button", { name: "Retry" })).toHaveCount(0);
    } finally { await service.remove(id); }
  });
});
