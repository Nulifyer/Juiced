import { api, capture, expect, hotModel, modelId, navigate, noHorizontalOverflow, open, picture, sourceOrigin, speechModel, storedText, tab, test, tryModel, withVision } from "./support";

test.describe("Chat", () => {
  test("a streamed reply fills the thread, Stop cancels and New chat clears", async ({ page }) => {
    const prompt = "Private chat journey prompt";
    await open(page);
    await navigate(page, "Test services");
    await expect(tab(page, "Test mode", "Try a model")).toHaveAttribute("aria-selected", "true");
    await tryModel(page, hotModel);
    const chat = page.getByRole("region", { name: "Chat test" });
    await expect(chat).toContainText("Messages stay in this tab and aren't saved.");
    await expect(chat.getByRole("button", { name: "Send" })).toBeDisabled();
    await expect(page.getByRole("region", { name: "Try a model" })).toContainText("Loaded · up to 1024 tokens per reply");
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
    await expect(chat.getByRole("button", { name: "New chat" })).toHaveCount(0);
    await navigate(page, "Dashboard");
    await navigate(page, "Test services");
    await expect(page.getByRole("region", { name: "Chat test" }).getByRole("article")).toHaveCount(0);
  });

  test("renders Markdown replies with a reasoning row, and a vision model takes images", async ({ page }) => {
    await withVision(page, hotModel);
    const bodies: any[] = [];
    const reply = "| Engine | Backend |\n| --- | --- |\n| llama.cpp | ROCm |\n| whisper.cpp | CPU |\n\n```json\n{\"backend\": \"rocm\"}\n```";
    await page.route("**/v1/chat/completions", async (route) => {
      bodies.push(route.request().postDataJSON());
      const chunks = [{ choices: [{ index: 0, delta: { reasoning_content: "The picture shows a chart." } }] }, { choices: [{ index: 0, delta: { content: reply } }] }, { choices: [{ index: 0, delta: {}, finish_reason: "stop" }], usage: { completion_tokens: 42 } }];
      await route.fulfill({ status: 200, headers: { "content-type": "text/event-stream" }, body: chunks.map((chunk) => `data: ${JSON.stringify(chunk)}\n\n`).join("") + "data: [DONE]\n\n" });
    });
    await open(page);
    await navigate(page, "Test services");
    await tryModel(page, hotModel);
    await expect(page.getByRole("region", { name: "Try a model" })).toContainText("accepts images");
    const chat = page.getByRole("region", { name: "Chat test" });
    await chat.getByLabel("Image files").setInputFiles([{ name: "a.png", mimeType: "image/png", buffer: await picture(page) }, { name: "notes.txt", mimeType: "text/plain", buffer: Buffer.from("x") }]);
    await expect(chat.getByRole("list", { name: "Images to send" }).getByRole("img")).toHaveCount(1);
    await chat.getByRole("button", { name: "Remove a.png" }).click();
    await expect(chat.getByRole("list", { name: "Images to send" })).toHaveCount(0);
    await chat.getByLabel("Image files").setInputFiles({ name: "chart.png", mimeType: "image/png", buffer: await picture(page) });
    await expect(chat.getByRole("button", { name: "Send" })).toBeEnabled();
    await chat.getByRole("button", { name: "Send" }).click();
    const answer = chat.getByRole("article", { name: "Model reply" });
    await expect(answer.getByRole("table").getByRole("row")).toHaveCount(3);
    await expect(answer.getByRole("button", { name: "Copy code" })).toBeVisible();
    await expect(answer.getByRole("button", { name: /^Thought/ })).toHaveAttribute("aria-expanded", "false");
    await expect(answer).toContainText("42 tokens");
    expect(bodies[0].messages).toEqual([{ role: "user", content: [{ type: "image_url", image_url: { url: expect.stringMatching(/^data:image\/jpeg;base64,/) } }] }]);
    await noHorizontalOverflow(page, "chat markdown");
    await capture(page, "try-chat", async () => { await expect(answer.getByRole("table")).toBeVisible(); });

    await chat.getByLabel("Prompt").fill("And now?");
    await chat.getByLabel("Prompt").press("Enter");
    await expect(answer).toHaveCount(2);
    expect(bodies[1].messages[0].content[0].type).toBe("image_url");
    expect(bodies[1].messages[2]).toEqual({ role: "user", content: "And now?" });
  });

  test("a model without vision offers no image input", async ({ page }) => {
    await open(page);
    await navigate(page, "Test services");
    await tryModel(page, hotModel);
    const chat = page.getByRole("region", { name: "Chat test" });
    await expect(chat.getByLabel("Prompt")).toBeVisible();
    await expect(chat.getByRole("button", { name: "Add images" })).toHaveCount(0);
    await expect(page.getByRole("region", { name: "Try a model" })).not.toContainText("accepts images");
  });

  test("an On demand model loads on its first message", async ({ page, request }) => {
    await open(page);
    await navigate(page, "Test services");
    await tryModel(page, modelId);
    const chat = page.getByRole("region", { name: "Chat test" });
    await expect(page.getByRole("region", { name: "Try a model" })).toContainText("Loads on first use");
    await chat.getByLabel("Prompt").fill("Load on demand");
    await chat.getByRole("button", { name: "Send" }).click();
    await expect(chat.getByRole("article", { name: "Model reply" })).toContainText("199", { timeout: 20000 });
    expect((await api(request).model(modelId))!.loaded).toBe(true);
  });
});

test("Transcribe uploads a WAV with a language and supports error, retry and stop", async ({ page, request }) => {
  const audio = await (await request.get(`${sourceOrigin}/speech-fixture.wav`)).body();
  await open(page);
  await navigate(page, "Test services");
  await tryModel(page, speechModel);
  const panel = page.getByRole("region", { name: "Transcription test" });
  const run = panel.getByRole("button", { name: "Transcribe" });
  await expect(run).toBeDisabled();
  await panel.getByLabel("Audio file").setInputFiles({ name: "invalid.wav", mimeType: "audio/wav", buffer: Buffer.from("invalid content") });
  await expect(panel).toContainText("invalid.wav");
  await run.click();
  await expect(panel.getByRole("alert")).toContainText("WAV");

  await panel.getByLabel("Audio file").setInputFiles({ name: "existing.wav", mimeType: "audio/wav", buffer: audio });
  await panel.getByLabel("Audio language").fill("de");
  await expect(panel.getByRole("alert")).toHaveCount(0);
  const endpoint = "**/v1/audio/transcriptions";
  await page.route(endpoint, (route) => route.fulfill({ status: 503, json: { error: { code: "speech_unavailable", message: "Fixture speech worker unavailable. Retry the request." } } }));
  await run.click();
  await expect(panel.getByRole("alert")).toContainText("Fixture speech worker unavailable");
  await expect(panel.getByLabel("Audio language")).toHaveValue("de");
  await expect(panel).toContainText("existing.wav");
  await page.unroute(endpoint);

  let echo: unknown;
  await page.route(endpoint, async (route) => { const response = await route.fetch(); echo = await response.json(); await route.fulfill({ response }); });
  await run.click();
  await expect.poll(() => echo, { timeout: 20000 }).toBeTruthy();
  await page.unroute(endpoint);
  expect(echo).toMatchObject({ received_file: { bytes: audio.byteLength, filename: "existing.wav", content_type: "audio/wav" }, received_fields: { language: "de" } });
  const output = panel.locator("[aria-live=polite]");
  await expect(output).toContainText("Fixture speech λ", { timeout: 20000 });
  await expect(panel).toContainText(/Done in \d+\.\d\d s/);

  await panel.getByLabel("Audio language").fill("auto");
  await expect(output).toHaveCount(0);
  const aborted = page.waitForEvent("requestfailed", { predicate: (outgoing) => outgoing.url().endsWith("/v1/audio/transcriptions") });
  await run.click();
  await expect(panel.getByText("Transcribing…")).toBeVisible();
  await panel.getByRole("button", { name: "Stop" }).click();
  await aborted;
  await expect(panel.getByRole("alert")).toContainText("Stopped.");
  await expect(panel).not.toContainText("Fixture speech λ");

  await expect(run).toBeEnabled();
  expect(await storedText(page)).not.toContain("existing.wav");
});
