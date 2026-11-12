import type { Route } from "@playwright/test";
import { api, expect, hotModel, modelId, noHorizontalOverflow, open, speechModel, test } from "./support";

const sse = (chunks: unknown[]) => chunks.map((chunk) => `data: ${JSON.stringify(chunk)}\n\n`).join("") + "data: [DONE]\n\n";
const toolCall = (calls: { id: string; name: string; arguments: unknown }[]) => [
  { choices: [{ index: 0, delta: { role: "assistant", tool_calls: calls.map((call, index) => ({ index, id: call.id, type: "function", function: { name: call.name, arguments: "" } })) } }] },
  ...calls.map((call, index) => ({ choices: [{ index: 0, delta: { tool_calls: [{ index, function: { arguments: JSON.stringify(call.arguments) } }] } }] })),
  { choices: [{ index: 0, delta: {}, finish_reason: "tool_calls" }] },
];

test.describe("Assistant", () => {
  test("reads run automatically, changes wait for approval and the result reaches the service", async ({ page, request }) => {
    const service = api(request);
    const speech = await service.model(speechModel);
    expect(speech?.policy).toBe("on-demand");
    const bodies: any[] = [];
    const mcpCalls: string[] = [];
    page.on("request", (outgoing) => {
      if (new URL(outgoing.url()).pathname === "/mcp" && outgoing.postDataJSON()?.method === "tools/call") mcpCalls.push(outgoing.postDataJSON().params.name);
    });
    await page.route("**/v1/chat/completions", async (route: Route) => {
      const body = route.request().postDataJSON();
      bodies.push(body);
      const step = bodies.length;
      const reply = step === 1 ? toolCall([{ id: "read", name: "juiced_list_models", arguments: {} }])
        : step === 2 ? toolCall([{ id: "load", name: "juiced_load_model", arguments: { model: modelId } }, { id: "policy", name: "juiced_set_policy", arguments: { model: speechModel, policy: "disabled", expected_revision: speech!.revision } }])
        : [{ choices: [{ index: 0, delta: { role: "assistant", content: `${speechModel} is disabled now.` } }] }, { choices: [{ index: 0, delta: {}, finish_reason: "stop" }] }];
      await route.fulfill({ status: 200, headers: { "content-type": "text/event-stream", "cache-control": "no-store" }, body: sse(reply) });
    });

    await open(page);
    const toggle = page.getByRole("button", { name: "Assistant", exact: true });
    await toggle.click();
    await expect(toggle).toHaveAttribute("aria-expanded", "true");
    const drawer = page.getByRole("complementary", { name: "Assistant" });
    await expect(drawer.getByRole("heading", { name: "Assistant", level: 2, exact: true })).toBeVisible();
    await drawer.getByLabel("Assistant model").selectOption(hotModel);
    const input = drawer.getByLabel("Message the assistant");
    await input.fill("Free memory by disabling the speech model");
    await input.press("Enter");

    const load = drawer.getByRole("group", { name: "Proposed change" }).filter({ hasText: `Load ${modelId}?` });
    await expect(load.getByRole("button", { name: "Approve" })).toBeVisible();
    await expect(drawer.getByLabel("Tool step").filter({ hasText: "Checked models" })).toBeVisible();
    expect(bodies[0]).toMatchObject({ model: hotModel, stream: true });
    expect(bodies[0].messages[0].role).toBe("system");
    expect(bodies[0].messages[0].content).toContain("Juiced");
    const offered = bodies[0].tools.map((tool: any) => tool.function.name);
    expect(offered).toContain("juiced_set_policy");
    expect(offered.some((name: string) => name.startsWith("lemonade_"))).toBe(false);
    const listed = bodies[1].messages.find((message: any) => message.role === "tool" && message.tool_call_id === "read");
    expect(JSON.parse(listed.content).models.some((model: any) => model.id === speechModel)).toBe(true);
    expect(mcpCalls).toEqual(["juiced_status", "juiced_list_models"]);

    const shots = process.env.JUICED_ASSISTANT_SHOTS;
    if (shots) {
      const original = page.viewportSize()!;
      for (const [width, height] of [[390, 844], [1280, 900], [2000, 1195]] as const) {
        await page.setViewportSize({ width, height });
        await expect(load).toBeVisible();
        await page.screenshot({ path: `${shots}/assistant-${width}x${height}.png` });
      }
      await page.setViewportSize(original);
    }
    await noHorizontalOverflow(page, "assistant open");

    await load.getByRole("button", { name: "Skip" }).click();
    await expect(load).toContainText("Skipped");
    const policy = drawer.getByRole("group", { name: "Proposed change" }).filter({ hasText: `Set ${speechModel} to Disabled?` });
    await expect(policy.getByRole("button", { name: "Approve" })).toBeVisible();
    expect((await service.model(speechModel))?.policy).toBe("on-demand");
    expect((await service.model(modelId))?.loaded).toBe(false);
    expect(mcpCalls).not.toContain("juiced_set_policy");
    expect(mcpCalls).not.toContain("juiced_load_model");

    await policy.getByRole("button", { name: "Approve" }).click();
    await expect(policy).toContainText(`${speechModel} is now Disabled.`);
    await expect(policy).toContainText("Done");
    await expect(drawer.getByRole("article", { name: "Assistant reply" }).last()).toContainText(`${speechModel} is disabled now.`);
    expect((await service.model(speechModel))?.policy).toBe("disabled");
    expect(mcpCalls.filter((name) => name === "juiced_set_policy")).toHaveLength(1);
    const results = bodies[2].messages.filter((message: any) => message.role === "tool");
    expect(JSON.parse(results.find((message: any) => message.tool_call_id === "load").content)).toMatchObject({ skipped: true });
    expect(JSON.parse(results.find((message: any) => message.tool_call_id === "policy").content).model.policy).toBe("disabled");

    await page.reload();
    await page.getByRole("button", { name: "Assistant", exact: true }).click();
    await expect(page.getByRole("complementary", { name: "Assistant" }).getByRole("article", { name: "You" })).toContainText("Free memory by disabling the speech model");
    await page.getByRole("complementary", { name: "Assistant" }).getByRole("button", { name: "New chat" }).click();
    await page.getByRole("button", { name: "Close assistant" }).click();
    await expect(page.getByRole("complementary", { name: "Assistant" })).toBeHidden();
  });
});
