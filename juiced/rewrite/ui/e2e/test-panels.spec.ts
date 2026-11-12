import { api, expect, hotModel, modelId, navigate, noHorizontalOverflow, open, storedText, tab, test, tryModel } from "./support";

const embedModel = "fixture.embed-tiny";

test("Compare streams one prompt to two models with timings and Stop cancels both", async ({ page }) => {
  test.setTimeout(60000);
  await open(page);
  await navigate(page, "Test services");
  await tab(page, "Test mode", "Compare").click();
  const panel = page.getByRole("region", { name: "Compare models" });
  const compare = panel.getByRole("button", { name: "Compare" });
  await panel.getByLabel("Model 1").selectOption(hotModel);
  await panel.getByLabel("Model 2").selectOption(hotModel);
  await panel.getByLabel("Prompt").fill("Private compare prompt");
  await expect(compare).toBeDisabled();
  await panel.getByLabel("Model 2").selectOption(modelId);
  await expect(compare).toBeEnabled();

  const sent: any[] = [];
  const listener = (outgoing: { method(): string; url(): string; postDataJSON(): any }) => { if (outgoing.method() === "POST" && new URL(outgoing.url()).pathname === "/v1/chat/completions") sent.push(outgoing.postDataJSON()); };
  page.on("request", listener);
  await compare.click();
  const columns = [panel.getByRole("article", { name: hotModel, exact: true }), panel.getByRole("article", { name: modelId, exact: true })];
  await expect(panel.getByRole("article")).toHaveCount(2);
  await expect(panel.getByRole("button", { name: "Stop" })).toBeVisible();
  await expect(panel.getByLabel("Model 1")).toBeDisabled();
  for (const column of columns) await expect(column).toContainText("3 ", { timeout: 20000 });
  for (const column of columns) {
    await expect(column).toContainText("199", { timeout: 20000 });
    const values = column.getByRole("definition");
    await expect(values.nth(0)).toHaveText(/^\d+ ms$/);
    await expect(values.nth(1)).toHaveText(/^\d+\.\d s$/);
    await expect(values.nth(2)).toHaveText("67 tok/s");
    await expect(column.getByRole("alert")).toHaveCount(0);
  }
  await expect(panel.getByRole("button", { name: "Stop" })).toHaveCount(0);
  page.off("request", listener);
  expect(sent.map((body) => body.model).sort()).toEqual([hotModel, modelId].sort());
  for (const body of sent) expect(body).toMatchObject({ stream: true, messages: [{ role: "user", content: "Private compare prompt" }] });
  await noHorizontalOverflow(page, "Compare results");

  await expect(panel.getByLabel("Prompt")).toHaveValue("Private compare prompt");
  page.on("request", listener);
  await compare.click();
  for (const column of columns) await expect(column.getByRole("paragraph")).toHaveText(/^0 1 2 3 /, { timeout: 20000 });
  // Stop turns into the submit button while its click is still being handled, so a filled prompt starts a new comparison.
  await panel.getByLabel("Prompt").fill("");
  await panel.getByRole("button", { name: "Stop" }).click();
  for (const column of columns) {
    await expect(column.getByRole("alert")).toHaveText("Stopped.");
    await expect(column).not.toContainText("199");
    await expect(column.getByRole("definition").nth(1)).toHaveText("—");
  }
  await expect(compare).toBeDisabled();
  page.off("request", listener);
  expect(sent).toHaveLength(4);
  expect(await storedText(page)).not.toContain("Private compare prompt");
});

test("Compare Stop with the prompt still filled does not start another comparison", async ({ page }) => {
  await open(page);
  await navigate(page, "Test services");
  await tab(page, "Test mode", "Compare").click();
  const panel = page.getByRole("region", { name: "Compare models" });
  await panel.getByLabel("Model 1").selectOption(hotModel);
  await panel.getByLabel("Model 2").selectOption(modelId);
  await panel.getByLabel("Prompt").fill("Stop once");
  await panel.getByRole("button", { name: "Compare" }).click();
  const columns = panel.getByRole("article");
  await expect(columns).toHaveCount(2);
  for (const column of await columns.all()) await expect(column.getByRole("paragraph")).toHaveText(/^0 1 2 3 /, { timeout: 20000 });
  let restarted = 0;
  page.on("request", (outgoing) => { if (outgoing.method() === "POST" && new URL(outgoing.url()).pathname === "/v1/chat/completions") restarted++; });
  await panel.getByRole("button", { name: "Stop" }).click();
  await expect(panel.getByRole("button", { name: "Compare" })).toBeVisible();
  for (const column of await columns.all()) await expect(column.getByRole("alert")).toHaveText("Stopped.", { timeout: 2000 });
  expect(restarted).toBe(0);
});

test("Embeddings embeds each line and shows dimensions, a cosine grid and vector previews", async ({ page, request }) => {
  const service = api(request);
  const lines = ["A cat sits on the mat", "A kitten rests on the mat", "Stock prices fell today"];
  try {
    await open(page);
    await navigate(page, "Test services");
    const picker = page.getByRole("region", { name: "Try a model" }).getByLabel("Model");
    await expect(picker.locator('optgroup[label="Embeddings"] option', { hasText: hotModel })).toHaveCount(0);
    await tryModel(page, embedModel);
    const panel = page.getByRole("region", { name: "Embeddings test" });
    const embed = panel.getByRole("button", { name: "Embed" });
    await expect(embed).toBeDisabled();
    await panel.getByLabel("Texts, one per line").fill(`  ${lines[0]}\n\n${lines[1]}\n${lines[2]}  \n`);
    const sent = page.waitForRequest((outgoing) => outgoing.method() === "POST" && new URL(outgoing.url()).pathname === "/v1/embeddings");
    await embed.click();
    expect((await sent).postDataJSON()).toEqual({ model: embedModel, input: lines });
    const vectors: number[][] = (await service.post<any>("/v1/embeddings", { model: embedModel, input: lines })).data.map((item: any) => item.embedding);
    expect(vectors).toHaveLength(3);

    await expect(panel).toContainText(new RegExp(`3 vectors · ${vectors[0]!.length} dimensions · \\d+ ms`));
    const grid = panel.getByRole("table", { name: "Cosine similarity" });
    const rows = grid.getByRole("row");
    await expect(rows).toHaveCount(4);
    await expect(rows.first().getByRole("columnheader")).toHaveText(["", "1", "2", "3"]);
    const norm = (vector: number[]) => Math.hypot(...vector);
    const cosine = (left: number[], right: number[]) => left.reduce((sum, value, index) => sum + value * right[index]!, 0) / (norm(left) * norm(right));
    for (let row = 0; row < 3; row++) {
      await expect(rows.nth(row + 1).getByRole("rowheader")).toHaveText(`${row + 1}. ${lines[row]}`);
      await expect(rows.nth(row + 1).getByRole("cell")).toHaveText(vectors.map((vector) => cosine(vectors[row]!, vector).toFixed(2)));
      await expect(rows.nth(row + 1).getByRole("cell").nth(row)).toHaveText("1.00");
    }
    expect(new Set(vectors.map((vector) => vector.join(","))).size).toBe(3);
    const previews = panel.locator(".vector-preview li");
    await expect(previews).toHaveCount(3);
    for (let index = 0; index < 3; index++) {
      await expect(previews.nth(index).locator("code")).toHaveText(`[${vectors[index]!.slice(0, 6).map((value) => value.toFixed(4)).join(", ")}, …]`);
    }
    await noHorizontalOverflow(page, "Embeddings results");
    expect(await storedText(page)).not.toContain("kitten");
  } finally {
    await service.unload(embedModel);
  }
});
