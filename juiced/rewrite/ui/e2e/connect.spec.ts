import type { APIRequestContext, Page } from "@playwright/test";
import { api, expect, navigate, noHorizontalOverflow, open, tab, test, toasts } from "./support";

const protocols = (page: Page) => page.getByRole("list", { name: "Client protocols" });
const row = (page: Page, name: string) => protocols(page).getByRole("listitem", { name, exact: true });
const details = (page: Page, name: string) => page.getByRole("complementary", { name: `${name} details` });

async function connect(page: Page) {
  await open(page);
  await navigate(page, "Connect");
  await expect(tab(page, "Connect views", "Protocols")).toHaveAttribute("aria-selected", "true");
  await expect(row(page, "OpenAI")).toBeVisible();
}

test("Protocols show health, copyable client URLs and per-client snippets", async ({ page, context, baseURL }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"], { origin: baseURL });
  await connect(page);
  const origin = baseURL!.replace(/\/$/, "");
  for (const [name, state] of [["OpenAI", /Idle|Healthy/], ["Ollama", /Idle|Healthy/], ["MCP", /Idle|Healthy/], ["Wyoming", /Off/]] as const) {
    await expect(row(page, name)).toContainText(state);
  }
  await expect(row(page, "OpenAI").getByRole("switch", { name: "OpenAI enabled" })).toHaveAttribute("aria-checked", "true");
  const copy = row(page, "OpenAI").getByRole("button", { name: "Copy OpenAI URL" });
  await copy.click();
  await expect(copy).toHaveText("Copied");
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(`${origin}/v1`);

  const openai = details(page, "OpenAI");
  await expect(openai.getByRole("textbox", { name: "OpenAI URL" })).toHaveValue(`${origin}/v1`);
  await expect(openai).toContainText("GET /v1/models → 200");
  const example = openai.getByRole("region", { name: "Example request" });
  await expect(example.locator("pre")).toContainText(`curl ${origin}/v1/chat/completions`);
  await example.getByRole("group", { name: "Example language" }).getByRole("button", { name: "Python" }).click();
  await expect(example.locator("pre")).toContainText(`client = OpenAI(base_url="${origin}/v1"`);
  await example.getByRole("button", { name: "Copy example" }).click();
  expect(await page.evaluate(() => navigator.clipboard.readText())).toContain("from openai import OpenAI");

  await row(page, "Anthropic").getByRole("button", { name: "Anthropic", exact: true }).click();
  await expect(details(page, "Anthropic").locator("pre")).toContainText(`Anthropic(base_url="${origin}"`);
  await row(page, "MCP").getByRole("button", { name: "MCP", exact: true }).click();
  await expect(details(page, "MCP").locator("pre")).toContainText(`"url": "${origin}/mcp"`);
  await row(page, "Realtime").getByRole("button", { name: "Realtime", exact: true }).click();
  await expect(details(page, "Realtime").getByRole("textbox", { name: "Realtime URL" })).toHaveValue(`${origin.replace(/^http/, "ws")}/v1/realtime?intent=transcription`);
  await noHorizontalOverflow(page, "Connect protocols");
});

test("turning a protocol off rejects its routes and hides it from discovery", async ({ page, request }) => {
  try {
    await connect(page);
    const ollama = row(page, "Ollama");
    await ollama.getByRole("switch", { name: "Ollama enabled" }).click();
    await expect(toasts(page).getByRole("status")).toContainText("Ollama is off.");
    await expect(ollama.getByRole("switch", { name: "Ollama enabled" })).toHaveAttribute("aria-checked", "false");
    await expect(ollama).toContainText("Off");
    const rejected = await request.get("/api/tags");
    expect(rejected.status()).toBe(404);
    expect((await rejected.json()).code).toBe("protocol_disabled");
    await ollama.getByRole("button", { name: "Ollama", exact: true }).click();
    await expect(details(page, "Ollama")).toContainText("Not run while off");
    await tab(page, "Connect views", "Endpoints").click();
    const table = page.getByRole("table", { name: "Endpoints" });
    await expect(table.getByRole("row").filter({ hasText: "POST /mcp" })).toBeVisible();
    await expect(table.getByRole("row").filter({ hasText: "GET /api/tags" })).toHaveCount(0);
    await tab(page, "Connect views", "Protocols").click();
    await row(page, "Ollama").getByRole("switch", { name: "Ollama enabled" }).click();
    await expect(row(page, "Ollama").getByRole("switch", { name: "Ollama enabled" })).toHaveAttribute("aria-checked", "true");
    expect((await request.get("/api/tags")).status()).toBe(200);
  } finally {
    await api(request).post("/v1/juiced/protocols", { protocol: "ollama", enabled: true });
  }
});

test("Endpoints list every registered route with its authority", async ({ page }) => {
  await connect(page);
  await tab(page, "Connect views", "Endpoints").click();
  const table = page.getByRole("table", { name: "Endpoints" });
  await expect(table.getByRole("row").filter({ hasText: "GET /v1/health" })).toContainText("read");
  await expect(table.getByRole("row").filter({ hasText: /^POST \/v1\/juiced\/access\/keys\s*operator$/ })).toBeVisible();
  await expect(table.getByRole("row").filter({ hasText: "POST /mcp" })).toBeVisible();
  await expect(page.getByText("Management routes also answer under /api/v0, /api/v1 and /v0.", { exact: false })).toBeVisible();
});

test("the Wyoming server is configured in its details and switched from the list", async ({ page, request }) => {
  const service = api(request);
  type Wyoming = { configuration_generation: number; settings: Record<string, unknown>; listener: { listening: boolean; address: string | null } };
  const before = await service.json<Wyoming>("/v1/juiced/wyoming");
  try {
    await connect(page);
    await row(page, "Wyoming").getByRole("button", { name: "Wyoming", exact: true }).click();
    const panel = details(page, "Wyoming");
    await panel.getByLabel("Listen on").fill("127.0.0.1:0");
    await panel.getByLabel("Speech to text").selectOption("fixture.speech-tiny");
    await panel.getByRole("button", { name: "Save" }).click();
    await expect(toasts(page).getByRole("status")).toContainText("Wyoming settings saved.");
    const power = row(page, "Wyoming").getByRole("switch", { name: "Wyoming enabled" });
    await power.click();
    await expect(power).toHaveAttribute("aria-checked", "true");
    await expect.poll(async () => (await service.json<Wyoming>("/v1/juiced/wyoming")).listener.listening).toBe(true);
    const live = await service.json<Wyoming>("/v1/juiced/wyoming");
    expect(live.settings.stt_model).toBe("fixture.speech-tiny");
    await expect(panel).toContainText(`Listening on ${live.listener.address}`);
    await power.click();
    await expect(power).toHaveAttribute("aria-checked", "false");
    await expect.poll(async () => (await service.json<Wyoming>("/v1/juiced/wyoming")).listener.listening).toBe(false);
  } finally {
    const now = await service.json<Wyoming>("/v1/juiced/wyoming");
    await service.post("/v1/juiced/wyoming", { settings: before.settings, expected_generation: now.configuration_generation });
  }
});

async function revokeAll(request: APIRequestContext, secret: string | null) {
  const headers = secret ? { Authorization: `Bearer ${secret}` } : undefined;
  const view = await request.get("/v1/juiced/access", { headers });
  if (!view.ok()) return;
  const keys: { id: string; scope: string; source: string }[] = (await view.json()).keys;
  for (const key of keys.filter((key) => key.source === "issued").sort((a, b) => Number(a.scope === "operator") - Number(b.scope === "operator"))) {
    await request.post("/v1/juiced/access/keys/revoke", { data: { id: key.id }, headers });
  }
}

test("API keys are issued once, enforced by scope and revoked back to an open network", async ({ page, context, baseURL, request }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"], { origin: baseURL });
  let operator: string | null = null;
  try {
    await open(page);
    await navigate(page, "Connect");
    await tab(page, "Connect views", "API keys").click();
    await expect(page.getByRole("status").filter({ hasText: "Open to this network." })).toBeVisible();
    await expect(page.getByLabel("Scope", { exact: true })).toBeDisabled();
    await page.getByLabel("Name", { exact: true }).fill("Console");
    await page.getByRole("button", { name: "Create operator key" }).click();
    const reveal = page.getByRole("dialog", { name: "Copy the new key" });
    operator = await reveal.getByRole("textbox", { name: "New API key" }).inputValue();
    expect(operator).toMatch(/^juiced_[A-Za-z0-9_-]{43}$/);
    await expect(reveal).toContainText("This console now uses this key in this tab.");
    await reveal.getByRole("button", { name: "Done" }).click();
    await expect(page.getByRole("status").filter({ hasText: "Keys required." })).toBeVisible();
    const table = page.getByRole("table", { name: "API keys" });
    await expect(table.getByRole("row", { name: "Console" })).toContainText("This console");
    await expect(table.getByRole("row", { name: "Console" })).toContainText(operator.slice(0, 13));
    expect((await request.get("/v1/models")).status()).toBe(401);
    expect((await request.get("/v1/models", { headers: { Authorization: `Bearer ${operator}` } })).status()).toBe(200);
    await expect(page.getByRole("heading", { name: "Connect", level: 1 })).toBeVisible();
    await expect(page.locator(".notice.error")).toHaveCount(0);

    await page.getByLabel("Name", { exact: true }).fill("Home Assistant");
    await expect(page.getByLabel("Scope", { exact: true })).toBeEnabled();
    await page.getByLabel("Expires", { exact: true }).selectOption({ label: "30 days" });
    await page.getByRole("button", { name: "Create key" }).click();
    await reveal.getByRole("button", { name: "Copy new key" }).click();
    const client = await page.evaluate(() => navigator.clipboard.readText());
    expect(client).toMatch(/^juiced_/);
    await expect(reveal).not.toContainText("This console now uses");
    await reveal.getByRole("button", { name: "Done" }).click();
    const assistant = table.getByRole("row", { name: "Home Assistant" });
    await expect(assistant).toContainText("Inference");
    await expect(assistant).toContainText("Expires in 30 days");
    const bearer = { Authorization: `Bearer ${client}` };
    expect((await request.get("/v1/models", { headers: bearer })).status()).toBe(200);
    expect((await (await request.get("/v1/juiced/access", { headers: bearer })).json()).error.code).toBe("operator_required");
    await expect(assistant).not.toContainText("Never", { timeout: 15000 });

    await assistant.getByRole("button", { name: "Revoke" }).click();
    await page.getByRole("dialog", { name: "Revoke Home Assistant?" }).getByRole("button", { name: "Revoke key" }).click();
    await expect(assistant).toHaveCount(0);
    expect((await request.get("/v1/models", { headers: bearer })).status()).toBe(401);

    await table.getByRole("row", { name: "Console" }).getByRole("button", { name: "Revoke" }).click();
    const last = page.getByRole("dialog", { name: "Revoke Console?" });
    await expect(last).toContainText("It's the last key.");
    await last.getByRole("button", { name: "Revoke key" }).click();
    await expect(page.getByRole("status").filter({ hasText: "Open to this network." })).toBeVisible();
    expect((await request.get("/v1/models")).status()).toBe(200);
    await noHorizontalOverflow(page, "Connect API keys");
  } finally {
    await revokeAll(request, operator);
    await revokeAll(request, null);
  }
});
