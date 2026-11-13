import { api, expect, hotModel, navigate, open, openModel, sourceOrigin, speechModel, tab, test, toasts, tryModel } from "./support";

const project = () => test.info().project.name;

test("typed generation, runtime and extra arguments save to the model profile", async ({ page, request }) => {
  const service = api(request);
  const fixture = await service.source();
  const id = `browser.settings-${project()}`;
  const path = `/v1/models/${encodeURIComponent(id)}/options`;
  await service.remove(id);
  await service.register({ id, model_file: fixture.model_file, provider: "llama-cpp", options: { context_tokens: 1024 }, command: fixture.command, args: [...fixture.args, "--ctx-size", "1024", "--temp", "0.9", "--jinja"] });
  try {
    await open(page);
    const inspector = await openModel(page, id);
    await tab(page, "Model sections", "Settings").click();
    const panel = inspector.getByRole("region", { name: "Context and launch profile" });
    const temperature = panel.getByLabel("Temperature", { exact: true });
    await expect(temperature).toHaveAttribute("placeholder", "0.9 (profile)");
    await expect(panel.getByLabel("Top K", { exact: true })).toHaveAttribute("placeholder", "Default 40");
    await expect(panel.getByLabel("Extra arguments", { exact: true })).toHaveCount(0);
    await temperature.fill("0.6");
    await panel.getByLabel("Top K", { exact: true }).fill("20");
    await panel.getByLabel("Reasoning budget", { exact: true }).fill("0");
    await inspector.getByRole("switch", { name: "Show advanced settings" }).click();
    const extra = panel.getByLabel("Extra arguments", { exact: true });
    await expect(extra).toHaveValue("--jinja");
    await panel.getByLabel("Flash attention", { exact: true }).selectOption("on");
    await panel.getByLabel("K cache", { exact: true }).selectOption("q8_0");
    await panel.getByLabel("Physical batch", { exact: true }).fill("256");
    await extra.fill("--jinja --spec-type draft-mtp --chat-template-kwargs '{\"enable_thinking\":false}'");
    await expect(inspector.getByRole("tab", { name: /^Settings/ }).getByLabel("unsaved")).toBeVisible();
    await panel.getByRole("button", { name: "Save", exact: true }).click();
    await expect(toasts(page).getByRole("status")).toContainText(`Saved launch settings for ${id}.`);
    const saved = await service.json<{ saved: Record<string, unknown>; extra_args: string[]; effective_launch: Record<string, unknown> }>(path);
    expect(saved.saved).toMatchObject({ temperature: 0.6, top_k: 20, reasoning_budget: 0, flash_attention: "on", cache_type_k: "q8_0", ubatch_size: 256, context_tokens: 1024 });
    expect(saved.extra_args).toEqual(["--jinja", "--spec-type", "draft-mtp", "--chat-template-kwargs", '{"enable_thinking":false}']);
    await expect(panel.getByRole("button", { name: "Save", exact: true })).toHaveCount(0);
    await expect(temperature).toHaveValue("0.6");

    await extra.fill("--jinja --top-p 0.5");
    await panel.getByRole("button", { name: "Save", exact: true }).click();
    await expect(toasts(page).getByRole("alert")).toContainText("own setting");
    expect((await service.json<{ extra_args: string[] }>(path)).extra_args).toEqual(saved.extra_args);
    await panel.getByRole("button", { name: "Cancel", exact: true }).click();
    await expect(extra).toHaveValue(`--jinja --spec-type draft-mtp --chat-template-kwargs '{"enable_thinking":false}'`);
    await panel.getByLabel("Top P", { exact: true }).fill("2");
    await panel.getByRole("button", { name: "Save", exact: true }).click();
    await expect(panel.getByRole("alert")).toContainText("Top P must be a number from 0 to 1");
  } finally {
    await service.remove(id);
  }
});

test("image models edit their engine defaults", async ({ page, request }) => {
  const service = api(request);
  const fixture = await service.source();
  const id = `browser.image-${project()}`;
  const path = `/v1/models/${encodeURIComponent(id)}/options`;
  await service.remove(id);
  await service.register({ id, model_file: fixture.model_file, provider: "stable-diffusion-cpp", command: fixture.command, args: ["--listen-port", "{port}", "--model", "{model}", "--steps", "8", "-W", "1024", "-H", "1024"], operations: ["images/generations"] });
  try {
    await open(page);
    const inspector = await openModel(page, id);
    await tab(page, "Model sections", "Settings").click();
    const form = inspector.getByRole("form", { name: "Image defaults" });
    await expect(form.getByLabel("Steps", { exact: true })).toHaveValue("8");
    await form.getByLabel("Steps", { exact: true }).fill("4");
    await form.getByLabel("Width", { exact: true }).fill("");
    await form.getByLabel("Sampler", { exact: true }).fill("euler");
    await form.getByRole("button", { name: "Save", exact: true }).click();
    await expect(toasts(page).getByRole("status")).toContainText(`Saved image defaults for ${id}.`);
    expect((await service.json<{ image_defaults: Record<string, unknown> }>(path)).image_defaults).toMatchObject({ steps: 4, width: null, height: 1024, sampling_method: "euler" });
    await expect(form.getByRole("button", { name: "Save", exact: true })).toHaveCount(0);
    await form.getByLabel("Height", { exact: true }).fill("1001");
    await form.getByRole("button", { name: "Save", exact: true }).click();
    await expect(form.getByRole("alert")).toContainText("multiple of 8");
  } finally {
    await service.remove(id);
  }
});

test("selected models change policy together and the list sorts", async ({ page, request }) => {
  const service = api(request);
  const fixture = await service.source();
  const ids = [`browser.bulk-a-${project()}`, `browser.bulk-b-${project()}`];
  for (const id of ids) { await service.remove(id); await service.register({ id, model_file: fixture.model_file, command: fixture.command, args: fixture.args }); }
  try {
    await open(page);
    await navigate(page, "Models");
    const table = page.getByRole("table", { name: "Registered models" });
    for (const id of ids) await table.getByLabel(`Select ${id}`).check();
    const bar = page.getByRole("toolbar", { name: "Selected models" });
    await expect(bar).toContainText("2 selected");
    await expect(bar.getByRole("button", { name: /^Load/ })).toBeDisabled();
    await bar.getByLabel("Set loading policy for selected models").selectOption("on-demand");
    await expect(bar).toHaveCount(0);
    await expect.poll(async () => (await Promise.all(ids.map((id) => service.model(id)))).map((model) => model?.policy)).toEqual(["on-demand", "on-demand"]);
    for (const id of ids) await table.getByLabel(`Select ${id}`).check();
    await page.getByRole("toolbar", { name: "Selected models" }).getByRole("button", { name: "Clear" }).click();
    await expect(table.getByLabel(`Select ${ids[0]}`)).not.toBeChecked();
    if (project() === "desktop") {
      await table.getByRole("button", { name: "Model", exact: true }).click();
      await table.getByRole("button", { name: "Model", exact: true }).click();
      const names = await table.locator(".model-row:not(.file-row) .model-name").allTextContents();
      const sorted = await page.evaluate((list) => [...list].sort((left, right) => right.localeCompare(left)), names);
      expect(names).toEqual(sorted);
    }
    await page.getByRole("button", { name: "Check for updates", exact: true }).click();
    await expect(toasts(page)).toContainText(/updates|couldn't be checked|unavailable/i);
  } finally {
    for (const id of ids) await service.remove(id);
  }
});

test("chat tests send max tokens and the thinking switch", async ({ page }) => {
  await open(page);
  await navigate(page, "Test services");
  await tryModel(page, hotModel);
  await page.getByLabel("Max tokens per reply").fill("64");
  await page.getByRole("switch", { name: "Thinking" }).click();
  const chat = page.getByRole("region", { name: "Chat test" });
  await chat.getByLabel("Prompt").fill("Short answer");
  const sent = page.waitForRequest((outgoing) => outgoing.method() === "POST" && new URL(outgoing.url()).pathname === "/v1/chat/completions");
  await chat.getByRole("button", { name: "Send" }).click();
  expect((await sent).postDataJSON()).toMatchObject({ max_tokens: 64, chat_template_kwargs: { enable_thinking: false } });
  await page.getByRole("switch", { name: "Thinking" }).click();
  await expect(chat.getByRole("button", { name: "Send" })).toBeVisible({ timeout: 20000 });
  await chat.getByLabel("Prompt").fill("Again");
  const again = page.waitForRequest((outgoing) => outgoing.method() === "POST" && new URL(outgoing.url()).pathname === "/v1/chat/completions");
  await chat.getByLabel("Prompt").press("Enter");
  expect((await again).postDataJSON().chat_template_kwargs).toBeUndefined();
});

test("transcription converts other audio to 16 kHz mono WAV before upload", async ({ page }) => {
  const rate = 44100, frames = rate / 2;
  const wav = Buffer.alloc(44 + frames * 4);
  wav.write("RIFF", 0); wav.writeUInt32LE(36 + frames * 4, 4); wav.write("WAVEfmt ", 8); wav.writeUInt32LE(16, 16); wav.writeUInt16LE(1, 20); wav.writeUInt16LE(2, 22);
  wav.writeUInt32LE(rate, 24); wav.writeUInt32LE(rate * 4, 28); wav.writeUInt16LE(4, 32); wav.writeUInt16LE(16, 34); wav.write("data", 36); wav.writeUInt32LE(frames * 4, 40);
  for (let frame = 0; frame < frames; frame++) { const sample = Math.round(Math.sin(frame / 20) * 8000); wav.writeInt16LE(sample, 44 + frame * 4); wav.writeInt16LE(sample, 46 + frame * 4); }
  await open(page);
  await navigate(page, "Test services");
  await tryModel(page, speechModel);
  const panel = page.getByRole("region", { name: "Transcription test" });
  await expect(panel.getByRole("button", { name: "Record" })).toBeVisible();
  await panel.getByLabel("Audio file").setInputFiles({ name: "stereo.wav", mimeType: "audio/wav", buffer: wav });
  await page.evaluate(() => {
    const original = window.fetch;
    window.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
      if (init?.body instanceof FormData) {
        const file = init.body.get("file") as File;
        (window as any).sentAudio = { name: file.name, header: Array.from(new Uint8Array(await file.slice(0, 44).arrayBuffer())) };
      }
      return original(input, init);
    }) as typeof fetch;
  });
  await panel.getByRole("button", { name: "Transcribe" }).click();
  await expect.poll(() => page.evaluate(() => (window as any).sentAudio ?? null)).not.toBeNull();
  const sent = await page.evaluate(() => (window as any).sentAudio as { name: string; header: number[] });
  const header = Buffer.from(sent.header);
  expect(header.toString("latin1", 8, 16)).toBe("WAVEfmt ");
  expect([header.readUInt16LE(22), header.readUInt32LE(24), header.readUInt16LE(34)]).toEqual([1, 16000, 16]);
  expect(sent.name).toBe("stereo.wav");
  await expect(panel.locator("[aria-live=polite]")).toContainText("Fixture speech", { timeout: 20000 });
});

test("cache limits save a disk budget and how long unused conversations stay", async ({ page, request }) => {
  const service = api(request);
  await open(page);
  await navigate(page, "Cache");
  const limits = page.getByRole("form", { name: "Cache limits" });
  await limits.getByLabel("Days to keep unused conversations").fill("30");
  await limits.getByLabel("Disk budget in GiB").fill("6");
  await limits.getByRole("button", { name: "Save" }).click();
  await expect(toasts(page).getByRole("status")).toContainText("Saved the conversation cache limits.");
  const settings = async () => (await service.json<{ settings: Record<string, unknown> }>("/v1/juiced/cache/sessions")).settings;
  expect(await settings()).toMatchObject({ retention_days: 30, budget_bytes: 6 * 1024 ** 3 });
  await limits.getByLabel("Days to keep unused conversations").fill("0");
  await limits.getByRole("button", { name: "Save" }).click();
  await expect(limits.getByRole("alert")).toContainText("1 to 365 days");
  await limits.getByLabel("Days to keep unused conversations").fill("");
  await limits.getByLabel("Disk budget in GiB").fill("8");
  await limits.getByRole("button", { name: "Save" }).click();
  await expect.poll(async () => (await settings()).retention_days).toBeUndefined();
  expect((await settings()).budget_bytes).toBe(8 * 1024 ** 3);
});
