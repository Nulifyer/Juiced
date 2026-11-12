import type { Page, Route } from "@playwright/test";
import { api, expect, hotModel, modelId, noHorizontalOverflow, open, speechModel, test } from "./support";

const sse = (chunks: unknown[]) => chunks.map((chunk) => `data: ${JSON.stringify(chunk)}\n\n`).join("") + "data: [DONE]\n\n";
const toolCall = (calls: { id: string; name: string; arguments: unknown }[]) => [
  { choices: [{ index: 0, delta: { role: "assistant", tool_calls: calls.map((call, index) => ({ index, id: call.id, type: "function", function: { name: call.name, arguments: "" } })) } }] },
  ...calls.map((call, index) => ({ choices: [{ index: 0, delta: { tool_calls: [{ index, function: { arguments: JSON.stringify(call.arguments) } }] } }] })),
  { choices: [{ index: 0, delta: {}, finish_reason: "tool_calls" }] },
];
const text = (content: string) => [{ choices: [{ index: 0, delta: { role: "assistant", content } }] }, { choices: [{ index: 0, delta: {}, finish_reason: "stop" }] }];

/** Scripts the chat model: each request gets the next reply. */
async function script(page: Page, replies: ((body: any) => unknown[])[]) {
  const bodies: any[] = [];
  await page.route("**/v1/chat/completions", async (route: Route) => {
    const body = route.request().postDataJSON();
    bodies.push(body);
    const reply = replies[Math.min(bodies.length, replies.length) - 1]!(body);
    await route.fulfill({ status: 200, headers: { "content-type": "text/event-stream", "cache-control": "no-store" }, body: sse(reply) });
  });
  return bodies;
}

function dispatches(page: Page) {
  const calls: { name: string; confirmed: boolean }[] = [];
  page.on("request", (outgoing) => {
    if (new URL(outgoing.url()).pathname === "/v1/juiced/barista/tools/call") { const body = outgoing.postDataJSON(); calls.push({ name: body.name, confirmed: body.confirmed }); }
  });
  return calls;
}

async function shots(page: Page, name: string, ready: () => Promise<void>) {
  const directory = process.env.JUICED_BARISTA_SHOTS;
  if (!directory) return;
  const original = page.viewportSize()!;
  for (const [width, height] of [[390, 844], [1280, 900], [2000, 1195]] as const) {
    await page.setViewportSize({ width, height });
    await ready();
    await page.screenshot({ path: `${directory}/barista-${name}-${width}x${height}.png` });
  }
  await page.setViewportSize(original);
}

async function ask(page: Page, message: string) {
  await page.getByRole("button", { name: "Barista", exact: true }).click();
  const panel = page.getByRole("complementary", { name: "Barista" });
  const input = panel.getByLabel("Message Barista");
  await input.fill(message);
  await input.press("Enter");
  return panel;
}

test.describe("Barista", () => {
  test("starts off, and Not now hides the invitation on this client without changing the service", async ({ page, request }) => {
    const service = api(request);
    await open(page);
    const invite = page.getByRole("group", { name: "Barista invitation" });
    await expect(invite.getByRole("button", { name: "Enable Barista" })).toBeVisible();
    await expect(page.getByRole("button", { name: "Barista", exact: true })).toHaveCount(0);
    await shots(page, "invitation", async () => { await expect(invite).toBeVisible(); });
    await noHorizontalOverflow(page, "invitation");

    await invite.getByRole("button", { name: "Not now" }).click();
    await expect(invite).toHaveCount(0);
    await expect(page.getByRole("button", { name: "Barista", exact: true })).toBeVisible();
    expect((await service.barista()).enabled).toBe(false);
    await page.reload();
    await expect(page.getByRole("heading", { name: "Dashboard", level: 1 })).toBeVisible();
    await expect(page.getByRole("button", { name: "Barista", exact: true })).toBeVisible();
    await expect(invite).toHaveCount(0);

    await page.getByRole("button", { name: "Barista", exact: true }).click();
    await expect(page.getByRole("form", { name: "Set up Barista" })).toBeVisible();
    expect((await service.barista()).enabled).toBe(false);
  });

  test("setup turns on a Disabled model explicitly, then settings change the permission", async ({ page, request }) => {
    const service = api(request);
    await service.policy(modelId, "disabled");
    await open(page);
    await page.getByRole("group", { name: "Barista invitation" }).getByRole("button", { name: "Enable Barista" }).click();
    const panel = page.getByRole("complementary", { name: "Barista" });
    await expect(panel.getByRole("heading", { name: "Barista", level: 2 })).toBeVisible();
    const setup = panel.getByRole("form", { name: "Set up Barista" });
    await expect(setup.getByLabel("Model", { exact: true })).toHaveValue(hotModel);
    await expect(setup).toContainText("Hot, so Juiced keeps it loaded.");
    await setup.getByLabel("Model", { exact: true }).selectOption(modelId);
    await expect(setup).toContainText(`${modelId} is Disabled. Turning Barista on sets it to On demand.`);
    await expect(setup).toContainText("Not yet tested with Barista.");
    await expect(setup.getByRole("radio", { name: "Full control" })).toHaveAttribute("aria-checked", "true");
    await shots(page, "setup", async () => { await expect(setup).toBeVisible(); });
    await noHorizontalOverflow(page, "setup");
    expect((await service.barista()).enabled).toBe(false);

    await setup.getByRole("button", { name: "Set to On demand and turn on" }).click();
    const status = panel.getByLabel("Barista status");
    await expect(status).toContainText(modelId);
    await expect(status).toContainText("Full control");
    await expect(status).toContainText("Ready");
    expect(await service.barista()).toMatchObject({ enabled: true, model: modelId, permission: "full" });
    expect((await service.model(modelId))?.policy).toBe("on-demand");
    expect((await service.model(modelId))?.loaded).toBe(false);
    await expect(page.getByRole("group", { name: "Barista invitation" })).toHaveCount(0);

    await panel.getByRole("button", { name: "Barista settings" }).click();
    const settings = panel.getByRole("form", { name: "Barista settings" });
    await settings.getByRole("radio", { name: "Read only" }).click();
    await expect(settings).toContainText("Barista can look but cannot change anything.");
    await settings.getByRole("button", { name: "Save" }).click();
    await expect(status).toContainText("Read only");
    expect((await service.barista()).permission).toBe("read_only");
  });

  test("full control runs a change without a prompt and reports it", async ({ page, request }) => {
    const service = api(request);
    await service.setBarista(true, hotModel, "full");
    const speech = await service.model(speechModel);
    const calls = dispatches(page);
    const bodies = await script(page, [
      () => toolCall([{ id: "read", name: "juiced_list_models", arguments: {} }]),
      () => toolCall([{ id: "policy", name: "juiced_set_policy", arguments: { model: speechModel, policy: "disabled", expected_revision: speech!.revision } }]),
      () => text(`${speechModel} is Disabled now.`),
    ]);
    await open(page);
    const panel = await ask(page, "Disable the speech model");
    const change = panel.getByRole("group", { name: "Change" }).filter({ hasText: `Set ${speechModel} to Disabled` });
    await expect(change).toContainText(`${speechModel} is now Disabled.`);
    await expect(change).toContainText("Done");
    await expect(change.getByRole("button", { name: "Approve" })).toHaveCount(0);
    await expect(panel.getByRole("article", { name: "Barista reply" }).last()).toContainText(`${speechModel} is Disabled now.`);
    expect((await service.model(speechModel))?.policy).toBe("disabled");
    expect(calls).toEqual([{ name: "juiced_status", confirmed: false }, { name: "juiced_list_models", confirmed: false }, { name: "juiced_set_policy", confirmed: false }]);

    expect(bodies[0]).toMatchObject({ model: hotModel, stream: true });
    const system = bodies[0].messages[0];
    expect(system.role).toBe("system");
    expect(system.content).toContain("You are Barista");
    expect(system.content).toContain(`You run on the model ${hotModel}.`);
    expect(system.content).toContain("Permission: Full control.");
    const offered = bodies[0].tools.map((tool: any) => tool.function.name);
    expect(offered).toContain("juiced_set_policy");
    expect(offered.some((name: string) => name.startsWith("lemonade_"))).toBe(false);
    const listed = bodies[1].messages.find((message: any) => message.role === "tool" && message.tool_call_id === "read");
    expect(JSON.parse(listed.content).models.some((model: any) => model.id === speechModel)).toBe(true);

    await shots(page, "panel", async () => { await expect(change).toBeVisible(); });
    await noHorizontalOverflow(page, "panel open");
    await page.reload();
    await page.getByRole("button", { name: "Barista", exact: true }).click();
    await expect(panel.getByRole("article", { name: "You" })).toContainText("Disable the speech model");
    await panel.getByRole("button", { name: "Clear" }).click();
    await expect(panel.getByRole("article", { name: "You" })).toHaveCount(0);
    await panel.getByRole("button", { name: "Close Barista" }).click();
    await expect(panel).toBeHidden();
  });

  test("ask before changes waits for approval and sends only approved changes", async ({ page, request }) => {
    const service = api(request);
    await service.setBarista(true, hotModel, "ask");
    const speech = await service.model(speechModel);
    const calls = dispatches(page);
    const bodies = await script(page, [
      () => toolCall([{ id: "load", name: "juiced_load_model", arguments: { model: modelId } }, { id: "policy", name: "juiced_set_policy", arguments: { model: speechModel, policy: "disabled", expected_revision: speech!.revision } }]),
      () => text("Done."),
    ]);
    await open(page);
    const panel = await ask(page, "Free memory by disabling the speech model");
    const load = panel.getByRole("group", { name: "Change" }).filter({ hasText: `Load ${modelId}?` });
    await expect(load.getByRole("button", { name: "Approve" })).toBeVisible();
    await expect(panel.getByLabel("Barista status")).toContainText("Waiting for your approval");
    expect(bodies[0].messages[0].content).toContain("Permission: Ask before changes.");
    await load.getByRole("button", { name: "Skip" }).click();
    await expect(load).toContainText("Skipped");
    const policy = panel.getByRole("group", { name: "Change" }).filter({ hasText: `Set ${speechModel} to Disabled?` });
    await expect(policy.getByRole("button", { name: "Approve" })).toBeVisible();
    expect((await service.model(speechModel))?.policy).toBe("on-demand");
    expect((await service.model(modelId))?.loaded).toBe(false);

    await policy.getByRole("button", { name: "Approve" }).click();
    await expect(policy).toContainText(`${speechModel} is now Disabled.`);
    expect((await service.model(speechModel))?.policy).toBe("disabled");
    expect(calls.filter((call) => call.name !== "juiced_status")).toEqual([{ name: "juiced_set_policy", confirmed: true }]);
    await expect(panel.getByRole("article", { name: "Barista reply" }).last()).toContainText("Done.");
    const results = bodies[1].messages.filter((message: any) => message.role === "tool");
    expect(JSON.parse(results.find((message: any) => message.tool_call_id === "load").content)).toMatchObject({ skipped: true });

    const current = await service.barista();
    const unconfirmed = await request.post("/v1/juiced/barista/tools/call", { data: { name: "juiced_load_model", arguments: { model: modelId }, revision: current.revision } });
    expect(unconfirmed.status()).toBe(403);
    expect((await unconfirmed.json()).error.code).toBe("assistant_confirmation_required");
  });

  test("read only offers no change tools and the service refuses one anyway", async ({ page, request }) => {
    const service = api(request);
    await service.setBarista(true, hotModel, "read_only");
    const speech = await service.model(speechModel);
    const bodies = await script(page, [
      () => toolCall([{ id: "policy", name: "juiced_set_policy", arguments: { model: speechModel, policy: "disabled", expected_revision: speech!.revision } }]),
      () => text("I can only look."),
    ]);
    await open(page);
    const panel = await ask(page, "Disable the speech model");
    const change = panel.getByRole("group", { name: "Change" }).filter({ hasText: `Set ${speechModel} to Disabled` });
    await expect(change).toContainText("Failed");
    await expect(change).toContainText("Barista is read only. It cannot make changes.");
    await expect(panel.getByRole("article", { name: "Barista reply" }).last()).toContainText("I can only look.");
    const offered = bodies[0].tools.map((tool: any) => tool.function.name);
    expect(offered).toContain("juiced_list_models");
    expect(offered).not.toContain("juiced_set_policy");
    expect(offered).not.toContain("juiced_load_model");
    expect(bodies[0].messages[0].content).toContain("Permission: Read only.");
    expect((await service.model(speechModel))?.policy).toBe("on-demand");
  });

  test("turning Barista off stops its request and refuses later tool calls", async ({ page, request }) => {
    const service = api(request);
    await service.setBarista(true, hotModel, "ask");
    const calls = dispatches(page);
    const bodies = await script(page, [() => toolCall([{ id: "load", name: "juiced_load_model", arguments: { model: modelId } }]), () => text("Should not run.")]);
    await open(page);
    const panel = await ask(page, "Load the long context model");
    const load = panel.getByRole("group", { name: "Change" }).filter({ hasText: `Load ${modelId}?` });
    await expect(load.getByRole("button", { name: "Approve" })).toBeVisible();
    const revision = (await service.barista()).revision;

    await panel.getByRole("button", { name: "Barista settings" }).click();
    await panel.getByRole("form", { name: "Barista settings" }).getByRole("button", { name: "Turn off Barista" }).click();
    const setup = panel.getByRole("form", { name: "Set up Barista" });
    await expect(setup.getByRole("button", { name: "Turn on Barista" })).toBeVisible();
    expect((await service.barista()).enabled).toBe(false);
    expect(bodies).toHaveLength(1);
    expect(calls.map((call) => call.name)).toEqual(["juiced_status"]);
    expect((await service.model(modelId))?.loaded).toBe(false);
    expect((await service.model(hotModel))?.loaded).toBe(true);
    const refused = await request.post("/v1/juiced/barista/tools/call", { data: { name: "juiced_list_models", arguments: {}, revision } });
    expect(refused.status()).toBe(409);
    expect((await refused.json()).error.code).toBe("assistant_disabled");

    await setup.getByRole("button", { name: "Turn on Barista" }).click();
    await expect(panel.getByRole("status").filter({ hasText: "Stopped. Changes that already finished stay in place." })).toBeVisible();
    await expect(load).toContainText("Not run.");
  });
});
