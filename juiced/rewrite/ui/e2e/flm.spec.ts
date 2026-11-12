import { expect, modelsView, open, test, toasts } from "./support";

test("NPU models from FastFlowLM download and add from Discover when FLM is installed", async ({ page }) => {
  let state = { installed: false, registered: [] as string[], pulling: false };
  const posts: { path: string; body: any }[] = [];
  await page.route("**/v1/juiced/flm**", async (route) => {
    const url = new URL(route.request().url());
    if (route.request().method() === "POST") {
      const body = route.request().postDataJSON();
      posts.push({ path: url.pathname, body });
      if (url.pathname.endsWith("/flm/pull")) { state = { ...state, pulling: true }; return route.fulfill({ status: 202, json: { job: { id: "flm-1", checkpoint: body.checkpoint, state: "pulling", percent: 10 } } }); }
      state = { ...state, registered: ["gemma3-1b-FLM"] };
      return route.fulfill({ json: { schema_version: 1, model: { id: "gemma3-1b-FLM" }, engine: "fastflowlm", backend: "npu" } });
    }
    return route.fulfill({ json: { schema_version: 1, available: true, validation: { ready: false, problems: ["Memlock limits are too low."] }, list_error: null,
      models: [{ checkpoint: "gemma3:1b", installed: state.installed, footprint_gb: 1.2, kind: "chat", registered: state.registered }, { checkpoint: "qwen3:8b", installed: false, footprint_gb: 5.6, kind: "chat", registered: [] }, { checkpoint: "embed-gemma:300m", installed: false, footprint_gb: 0.3, kind: "embeddings", registered: [] }],
      jobs: state.pulling && !state.installed ? [{ id: "flm-1", checkpoint: "gemma3:1b", state: "pulling", percent: 40 }] : [] } });
  });
  await open(page);
  await modelsView(page, "Discover");
  const section = page.getByRole("region", { name: "NPU models" });
  await expect(section).toContainText("Memlock limits are too low.");
  await expect(section).not.toContainText("embed-gemma");
  const row = section.getByRole("listitem").filter({ hasText: "gemma3:1b" });
  await row.getByRole("button", { name: "Download" }).click();
  await expect(toasts(page).getByRole("status")).toContainText("Downloading gemma3:1b.");
  await expect(row).toContainText("Downloading 40%");
  state = { ...state, installed: true, pulling: false };
  await expect(row.getByRole("button", { name: "Add" })).toBeVisible({ timeout: 15000 });
  await section.getByLabel("Add NPU models as").selectOption("hot");
  await row.getByRole("button", { name: "Add" }).click();
  await expect(toasts(page).getByRole("status")).toContainText("Added gemma3:1b on the NPU.");
  await expect(row).toContainText("Added");
  expect(posts.at(-1)).toEqual({ path: "/v1/juiced/flm/models/add", body: { checkpoint: "gemma3:1b", policy: "hot" } });
  await page.getByRole("searchbox", { name: "Search models" }).fill("qwen");
  await expect(section.getByRole("listitem")).toHaveCount(1);
});

test("the NPU section stays hidden when FastFlowLM is not installed", async ({ page }) => {
  await open(page);
  await modelsView(page, "Discover");
  await expect(page.getByRole("list", { name: "Search results" })).toBeVisible();
  await expect(page.getByRole("region", { name: "NPU models" })).toHaveCount(0);
});
