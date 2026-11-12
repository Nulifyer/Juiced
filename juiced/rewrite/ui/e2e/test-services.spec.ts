import { api, expect, hotModel, modelId, navigate, open, sourceOrigin, speechModel, storedText, tab, test } from "./support";

test.describe("Chat", () => {
  test("a streamed reply fills the thread, Stop cancels and New chat clears", async ({ page }) => {
    const prompt = "Private chat journey prompt";
    await open(page);
    await navigate(page, "Test services");
    await expect(tab(page, "Test operation", "Chat")).toHaveAttribute("aria-selected", "true");
    const chat = page.getByRole("region", { name: "Chat test" });
    await expect(chat).toContainText("Messages stay in this tab and aren't saved.");
    await expect(chat.getByRole("button", { name: "Send" })).toBeDisabled();
    await chat.getByLabel("Model").selectOption(hotModel);
    await expect(chat).toContainText("Loaded · up to 1024 tokens per reply");
    await chat.getByLabel("Prompt").fill(prompt);
    const sent = page.waitForRequest((outgoing) => outgoing.method() === "POST" && new URL(outgoing.url()).pathname === "/v1/chat/completions");
    await chat.getByRole("button", { name: "Send" }).click();
    const body = (await sent).postDataJSON();
    expect(body).toMatchObject({ model: hotModel, stream: true, messages: [{ role: "user", content: prompt }] });
    await expect(chat.getByLabel("Prompt")).toHaveValue("");
    await expect(chat.getByRole("article", { name: "You" })).toContainText(prompt);
    const reply = chat.getByRole("article", { name: "Model reply" });
    const stop = chat.getByRole("button", { name: "Stop" });
    await expect(reply).toContainText("3 ");
    await expect(stop).toBeVisible();
    await expect(reply).toContainText("199", { timeout: 15000 });
    await expect(stop).toHaveCount(0);
    await expect(reply).toContainText("tokens");

    await chat.getByLabel("Prompt").fill("Second private prompt");
    await chat.getByLabel("Prompt").press("Enter");
    const second = reply.nth(1);
    await expect(second).toContainText("2 ");
    await stop.click();
    await expect(second.getByRole("alert")).toHaveText("Stopped.");
    await expect(second).not.toContainText("199");
    await expect(chat.getByRole("button", { name: "Send" })).toBeVisible();
    expect(await storedText(page)).not.toContain("private");

    await chat.getByRole("button", { name: "New chat" }).click();
    await expect(chat.getByRole("article")).toHaveCount(0);
    await expect(chat).toContainText("Messages stay in this tab and aren't saved.");
    await expect(chat.getByRole("button", { name: "New chat" })).toBeDisabled();
    await navigate(page, "Dashboard");
    await navigate(page, "Test services");
    await expect(page.getByRole("region", { name: "Chat test" }).getByRole("article")).toHaveCount(0);
  });

  test("an On demand model loads on its first message", async ({ page, request }) => {
    await open(page);
    await navigate(page, "Test services");
    const chat = page.getByRole("region", { name: "Chat test" });
    await chat.getByLabel("Model").selectOption(modelId);
    await expect(chat).toContainText("Loads on first message");
    await chat.getByLabel("Prompt").fill("Load on demand");
    await chat.getByRole("button", { name: "Send" }).click();
    await expect(chat.getByRole("article", { name: "Model reply" })).toContainText("199", { timeout: 20000 });
    expect((await api(request).model(modelId))!.loaded).toBe(true);
  });
});

test("Transcribe uploads a WAV with a language and supports error, retry, stop and reset", async ({ page, request }) => {
  const audio = await (await request.get(`${sourceOrigin}/speech-fixture.wav`)).body();
  await open(page);
  await navigate(page, "Test services");
  await tab(page, "Test operation", "Transcribe").click();
  const panel = page.getByRole("region", { name: "Temporary transcription test" });
  const run = panel.getByRole("button", { name: "Transcribe audio" });
  await expect(run).toBeDisabled();
  await panel.getByLabel("Speech model").selectOption(speechModel);
  await panel.getByLabel("Temporary WAV file").setInputFiles({ name: "invalid.wav", mimeType: "audio/wav", buffer: Buffer.from("invalid content") });
  await expect(panel).toContainText("invalid.wav");
  await run.click();
  await expect(panel.getByRole("alert")).toContainText("WAV");

  await panel.getByLabel("Temporary WAV file").setInputFiles({ name: "existing.wav", mimeType: "audio/wav", buffer: audio });
  await panel.getByLabel("Audio language").fill("de");
  await expect(panel.getByRole("alert")).toHaveCount(0);
  const endpoint = "**/v1/audio/transcriptions";
  await page.route(endpoint, (route) => route.fulfill({ status: 503, json: { error: { code: "speech_unavailable", message: "Fixture speech worker unavailable. Retry the request." } } }));
  await run.click();
  await expect(panel.getByRole("alert")).toContainText("Fixture speech worker unavailable");
  await expect(panel.getByLabel("Audio language")).toHaveValue("de");
  await expect(panel).toContainText("existing.wav");
  await page.unroute(endpoint);

  const answered = page.waitForResponse((response) => response.request().method() === "POST" && response.url().endsWith("/v1/audio/transcriptions"));
  await run.click();
  const echo = await (await answered).json();
  expect(echo).toMatchObject({ received_file: { bytes: audio.byteLength, filename: "existing.wav", content_type: "audio/wav" }, received_fields: { language: "de" } });
  const output = panel.locator("[aria-live=polite]");
  await expect(panel.getByRole("status")).toHaveText("Done.", { timeout: 20000 });
  await expect(output).toContainText("Fixture speech λ");

  await panel.getByLabel("Audio language").fill("auto");
  await expect(output).toHaveCount(0);
  const aborted = page.waitForEvent("requestfailed", { predicate: (outgoing) => outgoing.url().endsWith("/v1/audio/transcriptions") });
  await run.click();
  await expect(panel.getByText("Transcribing…")).toBeVisible();
  await panel.getByRole("button", { name: "Stop test" }).click();
  await aborted;
  await expect(panel.getByRole("alert")).toContainText("Stopped.");
  await expect(panel).not.toContainText("Fixture speech λ");

  await panel.getByRole("button", { name: "Reset transcription" }).click();
  await expect(panel.getByLabel("Temporary WAV file")).toHaveValue("");
  await expect(panel.getByLabel("Audio language")).toHaveValue("auto");
  await expect(run).toBeDisabled();
  await expect(panel.getByRole("button", { name: "Reset transcription" })).toBeDisabled();
  expect(await storedText(page)).not.toContain("existing.wav");
});
