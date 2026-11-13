import { api, expect, hotModel, navigate, open, tab, test } from "./support";

test("Performance shows per-model timings, resource history and declared reservations", async ({ page, request }) => {
  const response = await request.post("/v1/chat/completions", { data: { model: hotModel, messages: [{ role: "user", content: "Performance fixture prompt" }], stream: false, max_tokens: 4 } });
  expect(response.ok()).toBe(true);
  await open(page);
  await navigate(page, "Diagnostics");
  await expect(page.getByRole("tablist", { name: "Diagnostics views" }).getByRole("tab")).toHaveText(["Performance", "Requests", "Logs"]);
  await expect(tab(page, "Diagnostics views", "Performance")).toHaveAttribute("aria-selected", "true");
  for (const chart of ["CPU", "Memory", "GPU", "VRAM"]) {
    await expect(page.getByRole("article", { name: chart })).toBeVisible();
    await expect(page.getByRole("img", { name: `${chart} over the last two minutes` })).toBeVisible();
  }
  await expect(page.getByRole("article", { name: "CPU" })).toContainText(/\d+(\.\d)?%/);
  const byModel = page.getByRole("table", { name: "Model performance" }).getByRole("row").filter({ hasText: hotModel });
  await expect(byModel).toContainText("43 tok/s");
  await expect(byModel).toContainText("prompt 700 tok/s");
  await expect(page.getByRole("article", { name: "Memory" })).toContainText(/GiB \/ \d+(\.\d)? GiB/);
  const reservations = page.getByRole("region", { name: "Reservations" });
  const panel = (await reservations.boundingBox())!;
  const cards = await Promise.all(["CPU", "VRAM"].map(async (chart) => (await page.getByRole("article", { name: chart }).boundingBox())!));
  expect(Math.abs(cards[0]!.x - panel.x), "the cards start with the panel below").toBeLessThan(2);
  expect(Math.abs(cards[1]!.x + cards[1]!.width - (panel.x + panel.width)), "the cards fill the row like the Dashboard").toBeLessThan(2);
  await expect(reservations).toContainText("Budget");
  await expect(reservations).toContainText("6.0 GiB");
  await expect(reservations).toContainText("Active requests");
  await page.getByRole("tablist", { name: "Diagnostics views" }).getByRole("tab", { name: "Performance" }).press("ArrowRight");
  await expect(tab(page, "Diagnostics views", "Requests")).toHaveAttribute("aria-selected", "true");
  await expect(tab(page, "Diagnostics views", "Requests")).toBeFocused();
});

test("Requests break down each request's time but never show prompt text", async ({ page, request }) => {
  const prompt = `Private diagnostics prompt ${crypto.randomUUID()}`;
  const response = await request.post("/v1/chat/completions", { data: { model: hotModel, messages: [{ role: "user", content: prompt }], stream: false, max_tokens: 4 } });
  expect(response.ok()).toBe(true);
  await open(page);
  await navigate(page, "Diagnostics");
  await tab(page, "Diagnostics views", "Requests").click();
  const legend = page.getByLabel("Time breakdown", { exact: true });
  await expect(legend).toContainText("Queue");
  await expect(legend).toContainText("Generate");
  const table = page.getByRole("table", { name: "Recent requests" });
  await expect(table.getByRole("columnheader", { includeHidden: true })).toHaveText(["Time", "Model", "Outcome", "Prompt", "Output", "Speed", "Duration"]);
  const row = table.getByRole("row").filter({ hasText: hotModel }).filter({ hasText: "completed · 200" }).first();
  await expect(row).toContainText("chat/completions");
  await expect(row.getByRole("cell").first()).toHaveText(/\d{1,2}:\d{2}:\d{2}/);
  await expect(row.getByRole("cell").nth(3)).toContainText("7");
  await expect(row.getByRole("cell").nth(4)).toHaveText("2");
  await expect(row.getByRole("cell").nth(5)).toContainText("43 tok/s");
  // Sub-millisecond phases are omitted from the label on fast machines.
  await expect(row.getByRole("img")).toHaveAttribute("aria-label", /^((Queue|Prompt|Generate|Other) )?\d+ ms( · (Prompt|Generate|Other) \d+ ms)*$/);
  await expect(row.getByRole("cell").last()).toContainText(/\d+ ms|\d+\.\d+ s/);
  await expect(page.locator("body")).not.toContainText(prompt);
  await expect(page.locator("body")).not.toContainText("Rust fixture reply");
});

test("Logs show service and worker lines with search, level, source and follow controls", async ({ page, request }) => {
  const service = api(request);
  const fixture = await service.source();
  const project = test.info().project.name;
  const marker = `journey-${project}-${crypto.randomUUID().slice(0, 8)}`;
  const chatty = `browser.logs-${project}`;
  const failing = `browser.logs-failing-${project}`;
  await service.remove(chatty);
  await service.remove(failing);
  try {
    await service.register({ id: chatty, model_file: fixture.model_file, command: fixture.command, args: [...fixture.args, "--startup-log", marker] });
    await service.register({ id: failing, model_file: fixture.model_file, command: fixture.command, args: [...fixture.args, "--mode", "exit"] });
    await service.policy(chatty, "on-demand");
    await service.policy(failing, "on-demand");
    await service.post("/v1/load", { model: chatty });
    expect((await request.post("/v1/load", { data: { model: failing } })).ok()).toBe(false);

    await open(page);
    await navigate(page, "Diagnostics");
    await tab(page, "Diagnostics views", "Logs").click();
    const log = page.getByRole("log", { name: "Log lines" });
    await expect(log).toContainText(`fixture stdout ${marker}`);
    await expect(log).toContainText(`W fixture stderr ${marker}`);
    await expect(log).toContainText(`Worker ${chatty} generation`);
    await expect(log).toContainText(`Worker ${failing} generation`);

    const search = page.getByRole("searchbox", { name: "Search logs" });
    await search.fill(marker);
    await expect(log).toContainText(`fixture stdout ${marker}`);
    await expect(log).not.toContainText("is ready");
    const levels = page.getByRole("group", { name: "Log level" });
    await levels.getByRole("button", { name: "Warnings" }).click();
    await expect(levels.getByRole("button", { name: "Warnings" })).toHaveAttribute("aria-pressed", "true");
    await expect(log).toContainText(`W fixture stderr ${marker}`);
    await expect(log).not.toContainText(`fixture stdout ${marker}`);
    await search.fill("");
    await levels.getByRole("button", { name: "Errors" }).click();
    await expect(log).toContainText(`Worker ${failing} generation`);
    await expect(log).toContainText("exited or missed its readiness deadline");
    await expect(log).not.toContainText(`W fixture stderr ${marker}`);
    await search.fill("no-such-log-line");
    await expect(log).toContainText("No lines match.");
    await search.fill("");
    await levels.getByRole("button", { name: "All" }).click();

    await page.getByLabel("Log source").selectOption(chatty);
    await expect(log).toContainText(`fixture stdout ${marker}`);
    await expect(log).not.toContainText(`Worker ${failing}`);
    await page.getByLabel("Log source").selectOption("service");
    await expect(log).toContainText(`Worker ${failing} generation`);
    await expect(log).not.toContainText(`fixture stdout ${marker}`);
    await page.getByLabel("Log source").selectOption("");

    const pause = page.getByRole("button", { name: "Pause", exact: true });
    await pause.click();
    await expect(page.getByRole("button", { name: "Follow", exact: true })).toBeVisible();
    await page.getByRole("button", { name: "Follow", exact: true }).click();
    await expect(pause).toBeVisible();
  } finally {
    await service.remove(chatty);
    await service.remove(failing);
  }
});
