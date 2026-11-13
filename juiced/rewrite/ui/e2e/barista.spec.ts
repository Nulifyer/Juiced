import type { Page, Route } from "@playwright/test";
import { api, capture, expect, hotModel, modelId, noHorizontalOverflow, open, picture, speechModel, test, withVision } from "./support";

const shots = (page: Page, name: string, ready: () => Promise<void>) => capture(page, `barista-${name}`, ready);

const sse = (chunks: unknown[]) => chunks.map((chunk) => `data: ${JSON.stringify(chunk)}\n\n`).join("") + "data: [DONE]\n\n";
const toolCall = (calls: { id: string; name: string; arguments: unknown }[]) => [
  { choices: [{ index: 0, delta: { role: "assistant", tool_calls: calls.map((call, index) => ({ index, id: call.id, type: "function", function: { name: call.name, arguments: "" } })) } }] },
  ...calls.map((call, index) => ({ choices: [{ index: 0, delta: { tool_calls: [{ index, function: { arguments: JSON.stringify(call.arguments) } }] } }] })),
  { choices: [{ index: 0, delta: {}, finish_reason: "tool_calls" }] },
];
const text = (content: string) => [{ choices: [{ index: 0, delta: { role: "assistant", content } }] }, { choices: [{ index: 0, delta: {}, finish_reason: "stop" }] }];
const thought = (reasoning: string, content: string) => [{ choices: [{ index: 0, delta: { role: "assistant", reasoning_content: reasoning } }] }, ...text(content)];
const rich = "Two models fit:\n\n| Model | Policy | Size |\n| --- | --- | ---: |\n| fixture.chat-small-q4 | Hot | 1.0 MiB |\n| fixture.chat-long-context-q8 | On demand | 1.0 MiB |\n\nTo load one from a shell:\n\n```bash\ncurl -X POST http://localhost:8080/v1/load -d '{\"model\":\"fixture.chat-long-context-q8\"}'\n```\n\nThe **Hot** model stays loaded.";


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
    await expect(page.getByRole("button", { name: "Barista", exact: true })).toHaveAttribute("aria-expanded", "true");
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
  test("renders tables and code, folds reasoning into one row and keeps tool steps compact", async ({ page, request }) => {
    const service = api(request);
    await service.setBarista(true, hotModel, "full");
    await script(page, [
      () => [...thought("The operator wants sizes. I should list models first.", "").slice(0, 1), ...toolCall([{ id: "read", name: "juiced_list_models", arguments: {} }])],
      () => thought("Both chat models are registered. A table is clearest.", rich),
    ]);
    await open(page);
    const panel = await ask(page, "Which chat models fit?");
    const reply = panel.getByRole("article", { name: "Barista reply" }).last();
    const table = reply.getByRole("table");
    await expect(table.getByRole("columnheader", { name: "Policy" })).toBeVisible();
    await expect(table.getByRole("row")).toHaveCount(3);
    await expect(reply).not.toContainText("| --- |");
    await expect(reply.locator(".md-code")).toContainText("curl -X POST");
    await expect(reply.getByRole("button", { name: "Copy code" })).toBeVisible();
    await expect(reply.locator("strong")).toHaveText("Hot");
    const step = panel.getByLabel("Tool step");
    await expect(step).toHaveText("Checked models");
    const reasoning = reply.getByRole("button", { name: /^Thought/ });
    await expect(reasoning).toHaveAttribute("aria-expanded", "false");
    await expect(reply).not.toContainText("A table is clearest.");
    await expect(panel.getByRole("button", { name: /^Thought/ })).toHaveCount(2);
    await shots(page, "rich", async () => { await expect(table).toBeVisible(); });
    await noHorizontalOverflow(page, "rich reply");
    await reasoning.click();
    await expect(reply).toContainText("A table is clearest.");
    await shots(page, "reasoning", async () => { await expect(reply).toContainText("A table is clearest."); });
  });

  test("opens straight to the chat once set up, and settings only behind the gear or when the model is gone", async ({ page, request }) => {
    const service = api(request);
    await service.setBarista(true, hotModel, "ask");
    await open(page);
    await page.getByRole("button", { name: "Barista", exact: true }).click();
    const panel = page.getByRole("complementary", { name: "Barista" });
    await expect(panel.getByLabel("Message Barista")).toBeFocused();
    await expect(panel.getByRole("form", { name: "Barista settings" })).toHaveCount(0);
    await panel.getByRole("button", { name: "Barista settings" }).click();
    await expect(panel.getByRole("form", { name: "Barista settings" })).toBeVisible();
    await expect(panel.getByLabel("Message Barista")).toHaveCount(0);
    await panel.getByRole("button", { name: "Barista settings" }).click();
    await expect(panel.getByLabel("Message Barista")).toBeVisible();

    await page.route("**/v1/juiced/catalog", async (route) => {
      const response = await route.fetch();
      const body = await response.json();
      await route.fulfill({ response, json: { ...body, data: body.data.filter((model: any) => model.id !== hotModel) } });
    });
    const settings = panel.getByRole("form", { name: "Barista settings" });
    await expect(settings).toBeVisible();
    await expect(panel.getByRole("status").filter({ hasText: `${hotModel} is no longer registered. Pick another model.` })).toBeVisible();
    await expect(panel.getByRole("button", { name: "Barista settings" })).toHaveCount(0);
    await settings.getByLabel("Model", { exact: true }).selectOption(modelId);
    await settings.getByRole("button", { name: "Save" }).click();
    await expect(panel.getByLabel("Barista status")).toContainText(modelId);
    await expect(panel.getByLabel("Message Barista")).toBeVisible();
    expect((await service.barista()).model).toBe(modelId);
  });

  test("pinning docks Barista beside the workspace and the choice stays on this client", async ({ page, request }, info) => {
    const service = api(request);
    await service.setBarista(true, hotModel, "full");
    await open(page);
    await page.getByRole("button", { name: "Barista", exact: true }).click();
    const panel = page.getByRole("complementary", { name: "Barista" });
    const pin = panel.getByRole("button", { name: "Pin Barista" });
    if (info.project.name !== "desktop") {
      await expect(pin).toHaveCount(0);
      const box = (await panel.boundingBox())!;
      expect(box.x).toBeLessThanOrEqual(page.viewportSize()!.width - box.width + 1);
      await shots(page, "unpinned", async () => { await expect(panel).toBeVisible(); });
      return;
    }
    const workspace = page.locator("main#workspace");
    const overlayWidth = (await workspace.boundingBox())!.width;
    await shots(page, "unpinned", async () => { await expect(panel).toBeVisible(); });
    await pin.click();
    await expect(pin).toHaveAttribute("aria-pressed", "true");
    await expect(page.locator(".app-shell")).toHaveClass(/barista-docked/);
    const docked = (await workspace.boundingBox())!;
    const box = (await panel.boundingBox())!;
    expect(docked.width).toBeLessThan(overlayWidth - 300);
    expect(docked.x + docked.width).toBeLessThanOrEqual(box.x + 1);
    expect(Math.round(box.x + box.width)).toBe(page.viewportSize()!.width);
    await page.keyboard.press("Escape");
    await expect(panel).toBeVisible();
    await noHorizontalOverflow(page, "pinned");
    await shots(page, "pinned", async () => { await expect(panel).toBeVisible(); });
    await page.setViewportSize({ width: 1280, height: 800 });

    await page.reload();
    await expect(panel).toBeVisible();
    await expect(page.locator(".app-shell")).toHaveClass(/barista-docked/);
    await page.setViewportSize({ width: 820, height: 900 });
    await expect(page.locator(".app-shell")).not.toHaveClass(/barista-docked/);
    await expect(panel.getByRole("button", { name: "Pin Barista" })).toHaveCount(0);
    await page.setViewportSize({ width: 1280, height: 800 });
    await panel.getByRole("button", { name: "Close Barista" }).click();
    await expect(panel).toBeHidden();
    await expect(page.locator(".app-shell")).not.toHaveClass(/barista-docked/);
    await page.reload();
    await expect(page.getByRole("heading", { name: "Dashboard", level: 1 })).toBeVisible();
    await expect(panel).toBeHidden();
    await page.getByRole("button", { name: "Barista", exact: true }).click();
    await expect(page.locator(".app-shell")).toHaveClass(/barista-docked/);
    await panel.getByRole("button", { name: "Pin Barista" }).click();
    await expect(page.locator(".app-shell")).not.toHaveClass(/barista-docked/);
    expect(await service.barista()).toMatchObject({ enabled: true, model: hotModel });
  });

  test("a vision model takes images as content parts, other models offer no image input", async ({ page, request }) => {
    const service = api(request);
    await service.setBarista(true, hotModel, "full");
    await open(page);
    await page.getByRole("button", { name: "Barista", exact: true }).click();
    const panel = page.getByRole("complementary", { name: "Barista" });
    await expect(panel.getByLabel("Message Barista")).toBeVisible();
    await expect(panel.getByRole("button", { name: "Add images" })).toHaveCount(0);

    await withVision(page, hotModel);
    const bodies = await script(page, [() => text("That screenshot shows the GPU at **72%**.")]);
    await expect(panel.getByRole("button", { name: "Add images" })).toBeVisible();
    await expect(panel.getByLabel("Barista status")).toContainText("Images");
    await panel.getByLabel("Image files").setInputFiles({ name: "usage.png", mimeType: "image/png", buffer: await picture(page) });
    const strip = panel.getByRole("list", { name: "Images to send" });
    await expect(strip.getByRole("img", { name: "usage.png" })).toBeVisible();
    await panel.getByLabel("Message Barista").fill("What does this show?");
    await shots(page, "image-attached", async () => { await expect(strip).toBeVisible(); });
    await panel.getByLabel("Message Barista").press("Enter");
    await expect(panel.getByRole("article", { name: "Barista reply" })).toContainText("That screenshot shows the GPU at 72%.");
    await expect(strip).toHaveCount(0);
    await expect(panel.getByRole("article", { name: "You" }).getByRole("img")).toBeVisible();
    const sent = bodies[0].messages.at(-1);
    expect(sent.role).toBe("user");
    expect(sent.content[0]).toEqual({ type: "text", text: "What does this show?" });
    expect(sent.content[1].type).toBe("image_url");
    expect(sent.content[1].image_url.url).toMatch(/^data:image\/jpeg;base64,/);
    await shots(page, "image-sent", async () => { await expect(panel.getByRole("article", { name: "You" }).getByRole("img")).toBeVisible(); });
  });
});
