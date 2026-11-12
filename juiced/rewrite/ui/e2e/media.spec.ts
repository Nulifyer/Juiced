import type { Page } from "@playwright/test";
import { expect, navigate, noHorizontalOverflow, open, tab, test } from "./support";

const png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";
const wav = () => {
  const samples = 800, buffer = new ArrayBuffer(44 + samples * 2), view = new DataView(buffer);
  const text = (offset: number, value: string) => [...value].forEach((character, index) => view.setUint8(offset + index, character.charCodeAt(0)));
  text(0, "RIFF"); view.setUint32(4, 36 + samples * 2, true); text(8, "WAVE"); text(12, "fmt "); view.setUint32(16, 16, true); view.setUint16(20, 1, true); view.setUint16(22, 1, true);
  view.setUint32(24, 8000, true); view.setUint32(28, 16000, true); view.setUint16(32, 2, true); view.setUint16(34, 16, true); text(36, "data"); view.setUint32(40, samples * 2, true);
  return Buffer.from(buffer);
};

async function mediaModels(page: Page, models: [string, string][]) {
  await page.route("**/v1/juiced/catalog", async (route) => {
    const response = await route.fetch();
    const json = await response.json();
    const template = json.data.find((model: { id: string }) => model.id === "fixture.chat-long-context-q8");
    json.data.push(...models.map(([id, operation]) => ({ ...template, id, operations: [operation], policy: "on-demand", loaded: false, state: "unloaded", pid: null, active_requests: 0 })));
    await route.fulfill({ response, json });
  });
}

test("Speak lists voices, sends the chosen voice, speed and format, and plays the reply", async ({ page }) => {
  const id = "browser.speech";
  await mediaModels(page, [[id, "audio/speech"]]);
  {
    const sent: any[] = [];
    await page.route("**/v1/juiced/voices?*", (route) => route.fulfill({ json: { schema_version: 1, data: [{ model: id, voices: ["af_heart", "am_michael", "bf_emma"], error: null }], aliases: {} } }));
    await page.route("**/v1/audio/speech", async (route) => { sent.push(route.request().postDataJSON()); await new Promise((done) => setTimeout(done, 300)); await route.fulfill({ body: wav(), contentType: "audio/wav" }); });
    await open(page);
    await navigate(page, "Test services");
    await tab(page, "Test operation", "Speak").click();
    const panel = page.getByRole("region", { name: "Speech test" });
    await panel.getByLabel("Model").selectOption(id);
    await expect(panel.getByLabel("Voice").locator("option")).toHaveCount(3);
    await panel.getByLabel("Voice").selectOption("am_michael");
    await panel.getByLabel("Speed").fill("1.5");
    await panel.getByRole("radio", { name: "WAV" }).click();
    await expect(panel.getByRole("button", { name: "Speak" })).toBeDisabled();
    await panel.getByLabel("Text").fill("  Hello from the journey.  ");
    await panel.getByRole("button", { name: "Speak" }).click();
    await expect(panel.getByRole("button", { name: "Stop" })).toBeVisible();
    await expect(panel.getByLabel("Generated speech")).toBeVisible();
    await expect(panel).toContainText(/Ready in \d+\.\d\d s/);
    expect(sent).toEqual([{ model: id, input: "Hello from the journey.", voice: "am_michael", speed: 1.5, response_format: "wav" }]);
    await expect(panel.getByRole("link", { name: "Download" })).toHaveAttribute("download", "speech.wav");

    await page.unroute("**/v1/audio/speech");
    await page.route("**/v1/audio/speech", (route) => route.fulfill({ status: 409, json: { error: { code: "voice_unknown", message: "The voice isn't in this model's voice pack." } } }));
    await panel.getByRole("button", { name: "Speak" }).click();
    await expect(panel.getByRole("alert")).toContainText("The voice isn't in this model's voice pack.");
    await expect(panel.getByLabel("Generated speech")).toHaveCount(0);
    await noHorizontalOverflow(page, "Speak test");
  }
});

test("Images remembers the size per model, validates fields, generates and clears results", async ({ page }) => {
  const first = "browser.image-a", second = "browser.image-b";
  await mediaModels(page, [[first, "images/generations"], [second, "images/generations"]]);
  {
    const sent: any[] = [];
    await page.route("**/v1/images/generations", async (route) => { sent.push(route.request().postDataJSON()); await new Promise((done) => setTimeout(done, 300)); await route.fulfill({ json: { created: 1, data: [{ b64_json: png }], output_format: "png" } }); });
    await open(page);
    await navigate(page, "Test services");
    await tab(page, "Test operation", "Images").click();
    const panel = page.getByRole("region", { name: "Image test" });
    await panel.getByLabel("Model").selectOption(first);
    await panel.getByLabel("Size").selectOption("768x512");
    await panel.getByLabel("Prompt").fill("a lighthouse at dusk");
    await panel.getByLabel("Steps").fill("200");
    await expect(panel).toContainText("Steps must be a whole number from 1 to 150.");
    await expect(panel.getByRole("button", { name: "Generate" })).toBeDisabled();
    await panel.getByLabel("Steps").fill("4");
    await panel.getByLabel("Seed").fill("42");
    await panel.getByLabel("Avoid").fill("fog");
    await panel.getByRole("button", { name: "Generate" }).click();
    await expect(panel.getByRole("button", { name: "Stop" })).toBeVisible();
    const results = panel.getByRole("list", { name: "Generated images" });
    await expect(results.getByRole("img", { name: "a lighthouse at dusk" })).toBeVisible();
    await expect(results).toContainText("768 × 512 · seed 42");
    expect(sent[0]).toEqual({ model: first, prompt: "a lighthouse at dusk", size: "768x512", seed: 42, negative_prompt: "fog", steps: 4 });

    await panel.getByLabel("Model").selectOption(second);
    await expect(results).toHaveCount(0);
    await expect(panel.getByLabel("Size")).toHaveValue("512x512");
    await panel.getByLabel("Model").selectOption(first);
    await expect(panel.getByLabel("Size")).toHaveValue("768x512");

    await panel.getByLabel("Seed").fill("");
    await panel.getByRole("button", { name: "Generate" }).click();
    await expect(results.getByRole("listitem")).toHaveCount(1);
    await panel.getByRole("button", { name: "Generate" }).click();
    await expect(results.getByRole("listitem")).toHaveCount(2);
    expect(Number.isInteger(sent.at(-1).seed)).toBe(true);
    await panel.getByRole("button", { name: "Clear" }).click();
    await expect(results).toHaveCount(0);

    await page.unroute("**/v1/images/generations");
    await page.route("**/v1/images/generations", (route) => route.fulfill({ status: 502, json: { error: { code: "worker_not_ready", message: "The engine exited or missed its readiness deadline." } } }));
    await panel.getByRole("button", { name: "Generate" }).click();
    await expect(panel.getByRole("alert")).toContainText("The engine exited or missed its readiness deadline.");
    await noHorizontalOverflow(page, "Images test");
  }
});
