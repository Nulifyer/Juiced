import { expect, modelsView, noHorizontalOverflow, open, test, toasts } from "./support";

test("NPU models sit after the results, ask the loading policy before download and add themselves when it ends", async ({ page }) => {
  let state = { pulled: false, registered: [] as string[], pulling: false };
  const posts: { path: string; body: any }[] = [];
  await page.route("**/v1/juiced/flm**", async (route) => {
    const url = new URL(route.request().url());
    if (route.request().method() === "POST") {
      const body = route.request().postDataJSON();
      posts.push({ path: url.pathname, body });
      if (url.pathname.endsWith("/flm/pull")) { state = { ...state, pulling: true }; return route.fulfill({ status: 202, json: { job: { id: "flm-1", checkpoint: body.checkpoint, state: "pulling", percent: 10, then: { kind: "add", policy: body.then.policy, state: "pending" } } } }); }
      return route.fulfill({ json: { schema_version: 1, model: { id: "qwen3-8b-FLM" }, engine: "fastflowlm", backend: "npu" } });
    }
    const job = state.pulling ? [{ id: "flm-1", checkpoint: "gemma3:1b", state: state.pulled ? "complete" : "pulling", percent: state.pulled ? 100 : 40, then: { kind: "add", policy: "hot", state: state.pulled ? "done" : "pending", model: state.pulled ? "gemma3-1b-FLM" : null } }] : [];
    return route.fulfill({ json: { schema_version: 1, available: true, validation: { ready: false, problems: ["Memlock limits are too low."] }, list_error: null,
      models: [{ checkpoint: "gemma3:1b", installed: state.pulled, footprint_gb: 1.2, kind: "chat", registered: state.registered }, { checkpoint: "qwen3:8b", installed: true, footprint_gb: 0.98, kind: "chat", registered: [] }, { checkpoint: "embed-gemma:300m", installed: false, footprint_gb: 0.3, kind: "embeddings", registered: [] }],
      jobs: job } });
  });
  await open(page);
  await modelsView(page, "Discover");
  const results = page.getByRole("list", { name: "Search results" });
  const section = page.getByRole("region", { name: "NPU models" });
  await expect(results).toBeVisible();
  await expect(section).toContainText("Memlock limits are too low.");
  expect((await section.boundingBox())!.y, "NPU models come after the search results").toBeGreaterThan((await results.boundingBox())!.y);
  await expect(section).not.toContainText("embed-gemma");
  await expect(section).not.toContainText(" GB");
  await expect(section.getByRole("listitem").filter({ hasText: "gemma3:1b" })).toContainText("1.2 GiB");
  await expect(section.getByRole("listitem").filter({ hasText: "qwen3:8b" })).toContainText("1.0 GiB");
  const policy = section.getByLabel("Loading policy for NPU models");
  await expect(policy).toHaveValue("on-demand");
  expect((await section.locator("header .inline-field").boundingBox())!.height, "the Loading policy label stays on one line").toBeLessThan(50);
  await noHorizontalOverflow(page, "Discover with NPU models");
  await policy.selectOption("hot");
  const row = section.getByRole("listitem").filter({ hasText: "gemma3:1b" });
  await row.getByRole("button", { name: "Download" }).click();
  expect(posts.at(-1)).toEqual({ path: "/v1/juiced/flm/pull", body: { checkpoint: "gemma3:1b", then: { policy: "hot" } } });
  await expect(toasts(page).getByRole("status")).toContainText("Downloading gemma3:1b. It's added when the download finishes.");
  await expect(row).toContainText("Downloading 40% · then Hot");
  state = { ...state, pulled: true, registered: ["gemma3-1b-FLM"] };
  await expect(row).toContainText("Added", { timeout: 15000 });
  expect(posts.filter((post) => post.path.endsWith("/flm/models/add")), "the service adds it, not the console").toHaveLength(0);
  const installed = section.getByRole("listitem").filter({ hasText: "qwen3:8b" });
  await installed.getByRole("button", { name: "Add" }).click();
  expect(posts.at(-1)).toEqual({ path: "/v1/juiced/flm/models/add", body: { checkpoint: "qwen3:8b", policy: "hot" } });

  const search = page.getByRole("searchbox", { name: "Search models" });
  await search.fill("qwen");
  await expect(section.getByRole("listitem")).toHaveCount(1);
  await search.fill("no-such-npu-model");
  await expect(section, "a search with no NPU match shows no NPU box").toHaveCount(0);
  await search.fill("");
  await page.getByRole("combobox", { name: "Engine" }).selectOption("flm");
  await expect(section).toBeVisible();
  await expect(results).toHaveCount(0);
});

test("the NPU section stays hidden when FastFlowLM is not installed", async ({ page }) => {
  await open(page);
  await modelsView(page, "Discover");
  await expect(page.getByRole("list", { name: "Search results" })).toBeVisible();
  await expect(page.getByRole("region", { name: "NPU models" })).toHaveCount(0);
});
