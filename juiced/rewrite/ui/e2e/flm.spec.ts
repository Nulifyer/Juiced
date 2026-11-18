import { expect, modelsView, noHorizontalOverflow, open, test, toasts } from "./support";

test("NPU models are results like any other: same row, detail, loading policy and download that adds itself", async ({ page }) => {
  let state = { pulled: false, registered: [] as string[], pulling: false, added: [] as string[] };
  const posts: { path: string; body: any }[] = [];
  await page.route("**/v1/juiced/flm**", async (route) => {
    const url = new URL(route.request().url());
    if (route.request().method() === "POST") {
      const body = route.request().postDataJSON();
      posts.push({ path: url.pathname, body });
      if (url.pathname.endsWith("/flm/pull")) { state = { ...state, pulling: true }; return route.fulfill({ status: 202, json: { job: { id: "flm-1", checkpoint: body.checkpoint, state: "pulling", percent: 10, then: { kind: "add", policy: body.then.policy, state: "pending" } } } }); }
      state = { ...state, added: ["qwen3-8b-FLM"] };
      return route.fulfill({ json: { schema_version: 1, model: { id: "qwen3-8b-FLM" }, engine: "fastflowlm", backend: "npu" } });
    }
    const job = state.pulling ? [{ id: "flm-1", checkpoint: "gemma3:1b", state: state.pulled ? "complete" : "pulling", percent: state.pulled ? 100 : 40, then: { kind: "add", policy: "hot", state: state.pulled ? "done" : "pending", model: state.pulled ? "gemma3-1b-FLM" : null } }] : [];
    return route.fulfill({ json: { schema_version: 1, available: true, validation: { ready: false, problems: ["Memlock limits are too low."] }, list_error: null,
      models: [{ checkpoint: "gemma3:1b", installed: state.pulled, footprint_gb: 1.2, kind: "chat", labels: [], registered: state.registered }, { checkpoint: "gemma3:4b", installed: false, footprint_gb: 4.4, kind: "chat", labels: ["vision"], registered: [] }, { checkpoint: "qwen3:8b", installed: true, footprint_gb: 0.98, kind: "chat", labels: ["reasoning"], registered: state.added }, { checkpoint: "embed-gemma:300m", installed: false, footprint_gb: 0.3, kind: "embeddings", labels: [], registered: [] }],
      jobs: job } });
  });
  await open(page);
  await modelsView(page, "Discover");
  const results = page.getByRole("list", { name: "Search results" });
  const card = (name: string) => results.getByRole("listitem").filter({ has: page.getByText(name, { exact: true }) });
  const type = page.getByRole("combobox", { name: "Type" });
  await expect(page.getByRole("list", { name: "Suggested models" }).getByRole("listitem").first()).toBeVisible({ timeout: 15000 });
  await expect(page.getByRole("list", { name: "Suggested models" }), "suggestions come from the catalog, not the NPU list").not.toContainText("FastFlowLM");
  await type.selectOption({ label: "Chat" });
  await expect(results.getByRole("listitem").filter({ hasText: "Qwen3 0.6B" }).first()).toBeVisible({ timeout: 15000 });
  await expect(page.getByRole("region", { name: "NPU models" }), "no separate NPU section").toHaveCount(0);
  const gemma = card("gemma3");
  await expect(gemma.locator(".result-meta")).toHaveText("FastFlowLM · Chat · NPU");
  const offered = gemma.locator(".result-quant");
  await expect(offered, "the row offers the largest size that fits").toHaveText(/^4b\s*4\.4 GiB$/);
  await expect(gemma.getByRole("button", { name: "2 sizes" })).toBeVisible();
  await expect(gemma.getByRole("button", { name: "Download gemma3 4b" })).toBeEnabled();
  await expect(card("qwen3").getByText("✓ On disk")).toBeVisible();
  await expect(card("qwen3").locator(".result-quant")).toContainText("1.0 GiB");
  await expect(card("qwen3").getByRole("button", { name: /^Download/ }), "a size on disk is not offered as a download").toHaveCount(0);
  await expect(results, "only chat checkpoints can be added").not.toContainText("embed-gemma");
  await expect(results).not.toContainText(" GB");
  await noHorizontalOverflow(page, "Discover with NPU results");

  const hardware = page.getByRole("combobox", { name: "Hardware" }), engine = page.getByRole("combobox", { name: "Engine" });
  await type.selectOption("");
  await hardware.selectOption({ label: "NPU" });
  await expect(results.getByRole("listitem")).toHaveCount(2);
  await expect(page.getByText("Memlock limits are too low.")).toBeVisible();
  await page.getByRole("combobox", { name: "Size" }).selectOption({ label: "Under 2 GiB" });
  await expect(offered, "size narrows NPU sizes like any other; the rest stay one step away").toHaveText(/^1b/);
  await expect(gemma.getByRole("button", { name: "2 sizes" })).toBeVisible();
  await page.getByRole("button", { name: "Clear filters" }).click();
  await expect(page.getByText("Memlock limits are too low."), "the NPU warning leaves with the NPU filter").toHaveCount(0);
  await engine.selectOption("flm");
  await expect(results.getByRole("listitem")).toHaveCount(2);
  await page.getByRole("combobox", { name: "Type" }).selectOption({ label: "Embeddings" });
  await expect(page.getByRole("heading", { name: "No models found" })).toBeVisible();
  await page.getByRole("button", { name: "Clear filters" }).click();
  const search = page.getByRole("searchbox", { name: "Search models" });
  await search.fill("gemma3");
  await expect(gemma).toBeVisible();
  await expect(card("qwen3")).toHaveCount(0);
  await search.fill("");

  await hardware.selectOption({ label: "NPU" });
  await gemma.getByRole("button", { name: "gemma3", exact: true }).click();
  const detail = page.getByRole("complementary", { name: "Model detail" });
  await expect(detail.getByRole("heading", { name: "gemma3" })).toBeVisible();
  const sizes = detail.getByRole("listbox", { name: "Sizes" });
  await expect(sizes.getByRole("option")).toHaveCount(2);
  await sizes.getByRole("option", { name: /1b/ }).click();
  await expect(detail.getByRole("definition").filter({ hasText: "NPU" })).toHaveText("Runs on the NPU");
  await expect(detail).toContainText("Memlock limits are too low.");
  const policy = detail.getByRole("radiogroup", { name: "Loading policy" });
  await expect(policy.getByRole("radio", { name: "On demand" })).toBeChecked();
  await policy.getByRole("radio", { name: "Hot" }).click();
  await detail.getByRole("button", { name: "Download" }).click();
  expect(posts.at(-1)).toEqual({ path: "/v1/juiced/flm/pull", body: { checkpoint: "gemma3:1b", then: { policy: "hot" } } });
  await expect(toasts(page).getByRole("status")).toContainText("Downloading gemma3:1b. It's installed as Hot when done.");
  await expect(detail).toContainText("Downloading 40% · then installed as Hot");
  await expect(sizes.getByRole("option", { name: /1b/ })).toContainText("Downloading 40%");
  await expect(detail.getByRole("button", { name: "Download" })).toHaveCount(0);
  state = { ...state, pulled: true, registered: ["gemma3-1b-FLM"] };
  await expect(detail.getByRole("status").filter({ hasText: "Installed gemma3-1b-FLM." })).toBeVisible({ timeout: 15000 });
  await expect(detail.getByRole("button", { name: "Open model" })).toBeVisible();
  expect(posts.filter((post) => post.path.endsWith("/flm/models/add")), "the service adds it, not the console").toHaveLength(0);
  await noHorizontalOverflow(page, "NPU model detail");

  const back = detail.getByRole("button", { name: "Back to results" });
  await (await back.isVisible() ? back : detail.getByRole("button", { name: "Close detail" })).click();
  await card("qwen3").getByRole("button", { name: "qwen3", exact: true }).click();
  await expect(detail.getByRole("definition").filter({ hasText: "reasoning" })).toBeVisible();
  await detail.getByRole("radiogroup", { name: "Loading policy" }).getByRole("radio", { name: "Disabled" }).click();
  await detail.getByRole("button", { name: "Install", exact: true }).click();
  expect(posts.at(-1)).toEqual({ path: "/v1/juiced/flm/models/add", body: { checkpoint: "qwen3:8b", policy: "disabled" } });
  await expect(detail.getByRole("status")).toContainText("Installed qwen3-8b-FLM. Disabled on NPU.");
});

test("without FastFlowLM there are no NPU results or NPU choices", async ({ page }) => {
  await open(page);
  await modelsView(page, "Discover");
  await page.getByRole("combobox", { name: "Type" }).selectOption({ label: "Chat" });
  await expect(page.getByRole("list", { name: "Search results" })).toBeVisible();
  await expect(page.locator(".result-meta").filter({ hasText: "NPU" })).toHaveCount(0);
  await expect(page.getByRole("combobox", { name: "Engine" }).getByRole("option", { name: /FastFlowLM/ })).toHaveCount(0);
  await expect(page.getByRole("combobox", { name: "Hardware" }).getByRole("option", { name: "NPU" })).toHaveCount(0);
});
