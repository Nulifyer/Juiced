import { expect, hotModel, navigate, open, openModel, test, toasts } from "./support";

test("a model with newer upstream files updates from its row and from the inspector", async ({ page }) => {
  let update: "none" | "pending" | "done" = "none";
  const posts: { path: string; body: any }[] = [];
  await page.route(/\/v1\/models(\?.*)?$/, async (route) => {
    let response, body;
    try { response = await route.fetch(); body = await response.json(); } catch { return route.abort().catch(() => undefined); }
    for (const model of body.data ?? []) if (model.id === hotModel) model.update_available = update !== "done";
    await route.fulfill({ response, json: body }).catch(() => undefined);
  });
  await page.route(/\/v1\/models\/[^/]+\/update$/, async (route) => {
    posts.push({ path: new URL(route.request().url()).pathname, body: route.request().postDataJSON() });
    update = "pending";
    await route.fulfill({ json: { pull: "update-1", state: "downloading", then: { kind: "update", model: hotModel, state: "pending" } } });
  });
  await page.route(/\/v1\/juiced\/library\/pulls$/, async (route) => {
    const data = update === "none" ? [] : [{ pull: "update-1", state: update === "done" ? "complete" : "downloading", repo: "fixture-labs/Tiny-Chat-GGUF", label: "Q4_K_M", error: null,
      files: [{ file: "Tiny-Chat-Q4_K_M.gguf", bytes: 1000, state: update === "done" ? "present" : "downloading", downloaded_bytes: update === "done" ? 1000 : 400, job: null }],
      then: { kind: "update", model: hotModel, from_artifact: "a".repeat(64), state: update === "done" ? "done" : "pending", error: null, message: null } }];
    await route.fulfill({ json: { schema_version: 1, data } });
  });
  await open(page);
  await navigate(page, "Models");
  const table = page.getByRole("table", { name: "Registered models" });
  const row = table.getByRole("row").filter({ has: page.getByRole("button", { name: hotModel, exact: true }) });
  await expect(row).toContainText("update available");
  await row.getByRole("button", { name: `Update ${hotModel}` }).click();
  expect(posts).toHaveLength(1);
  expect(posts[0]!.path).toBe(`/v1/models/${encodeURIComponent(hotModel)}/update`);
  expect(posts[0]!.body).toEqual({ expected_revision: expect.any(Number) });
  await expect(toasts(page).getByRole("status")).toContainText(`Updating ${hotModel}. Follow it under Downloads.`);
  await expect(row).toContainText("Updating 40%");

  const inspector = await openModel(page, hotModel);
  await expect(inspector).toContainText("Updating 40%");
  update = "done";
  await expect(inspector).not.toContainText("Updating", { timeout: 10000 });
  await expect(inspector.getByRole("button", { name: "Update" })).toHaveCount(0);
});
