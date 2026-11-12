import { api, expect, navigate, open, tab, test, toasts } from "./support";

test("Protocols list client addresses with Copy and switch example languages", async ({ page, context, baseURL }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"], { origin: baseURL });
  await open(page);
  await navigate(page, "Connect");
  await expect(tab(page, "Connect views", "Protocols")).toHaveAttribute("aria-selected", "true");
  const origin = baseURL!.replace(/\/$/, "");
  const addresses = { OpenAI: `${origin}/v1`, Anthropic: `${origin}/v1/messages`, Ollama: `${origin}/api`, MCP: `${origin}/mcp` };
  for (const [name, address] of Object.entries(addresses)) {
    const card = page.getByRole("article", { name, exact: true });
    await expect(card.getByRole("textbox", { name: `${name} address` })).toHaveValue(address);
    await expect(card).toContainText("partial");
  }
  await expect(page.getByRole("article", { name: "OpenAI", exact: true })).toContainText("chat/completions");
  await expect(page.getByRole("article", { name: "Realtime", exact: true }).getByRole("textbox", { name: "Realtime address" })).toHaveValue(`${origin.replace(/^http/, "ws")}/v1/realtime?intent=transcription`);
  await expect(page.getByText("Not available yet")).toHaveCount(0);
  const copy = page.getByRole("button", { name: "Copy OpenAI address" });
  await copy.click();
  await expect(copy).toHaveText("Copied");
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(addresses.OpenAI);
  await expect(copy).toHaveText("Copy", { timeout: 4000 });

  const example = page.getByRole("region", { name: "Example request" });
  const languages = example.getByRole("group", { name: "Example language" });
  await expect(languages.getByRole("button", { name: "curl" })).toHaveAttribute("aria-pressed", "true");
  await expect(example.locator("pre")).toContainText(`curl ${origin}/v1/chat/completions`);
  await languages.getByRole("button", { name: "Python" }).click();
  await expect(languages.getByRole("button", { name: "Python" })).toHaveAttribute("aria-pressed", "true");
  await expect(example.locator("pre")).toContainText(`client = OpenAI(base_url="${origin}/v1"`);
  await languages.getByRole("button", { name: "JavaScript" }).click();
  await expect(example.locator("pre")).toContainText(`new OpenAI({ baseURL: "${origin}/v1"`);
  await example.getByRole("button", { name: "Copy example" }).click();
  expect(await page.evaluate(() => navigator.clipboard.readText())).toContain('import OpenAI from "openai";');
});

test("Endpoints list every registered route with its authority", async ({ page }) => {
  await open(page);
  await navigate(page, "Connect");
  await tab(page, "Connect views", "Endpoints").click();
  const table = page.getByRole("table", { name: "Endpoints" });
  await expect(table.getByRole("row").filter({ hasText: "GET /v1/health" })).toContainText("read");
  await expect(table.getByRole("row").filter({ hasText: "POST /v1/juiced/cache/save" })).toBeVisible();
  await expect(table.getByRole("row").filter({ hasText: "POST /mcp" })).toBeVisible();
  await expect(table.getByRole("row").filter({ hasText: "GET /v1/system-info" })).toContainText("operator");
  await expect(page.getByText("Management routes also answer under /api/v0, /api/v1 and /v0.", { exact: false })).toBeVisible();
});

test("the Wyoming server is configured, turned on and off from Connect", async ({ page, request }) => {
  const service = api(request);
  type Wyoming = { configuration_generation: number; settings: Record<string, unknown>; listener: { listening: boolean; address: string | null } };
  const before = await service.json<Wyoming>("/v1/juiced/wyoming");
  try {
    await open(page);
    await navigate(page, "Connect");
    const card = page.getByRole("article", { name: "Wyoming", exact: true });
    const power = card.getByRole("radiogroup", { name: "Wyoming server" });
    await expect(power.getByRole("radio", { name: "Off" })).toHaveAttribute("aria-checked", "true");
    await card.getByLabel("Listen on").fill("127.0.0.1:0");
    await card.getByLabel("Speech to text").selectOption("fixture.speech-tiny");
    await expect(power.getByRole("radio", { name: "On" })).toBeDisabled();
    await card.getByRole("button", { name: "Save" }).click();
    await expect(toasts(page).getByRole("status")).toContainText("Wyoming settings saved.");
    await power.getByRole("radio", { name: "On" }).click();
    await expect(card.getByText("Listening", { exact: true })).toBeVisible();
    const live = await service.json<Wyoming>("/v1/juiced/wyoming");
    expect(live.listener.listening).toBe(true);
    expect(live.settings.stt_model).toBe("fixture.speech-tiny");
    await expect(card).toContainText(`host and port ${live.listener.address!.replace("127.0.0.1", "127.0.0.1")}`);
    await power.getByRole("radio", { name: "Off" }).click();
    await expect(card.getByText("Listening", { exact: true })).toBeHidden();
    expect((await service.json<Wyoming>("/v1/juiced/wyoming")).listener.listening).toBe(false);
  } finally {
    const now = await service.json<Wyoming>("/v1/juiced/wyoming");
    await service.post("/v1/juiced/wyoming", { settings: before.settings, expected_generation: now.configuration_generation });
  }
});
