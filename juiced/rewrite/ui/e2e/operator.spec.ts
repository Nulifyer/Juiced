import { expect, test, type APIRequestContext, type Page, type Route } from "@playwright/test";
import { randomUUID } from "node:crypto";
import { readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";

const sourceOrigin = `http://127.0.0.1:${process.env.JUICED_SOURCE_PORT ?? 13308}`;
const modelId = "fixture.chat-long-context-q8";
async function navigate(page: Page, name: string) {
  const menu = page.getByRole("button", { name: "Toggle navigation" });
  if (await menu.isVisible()) await menu.click();
  await page.getByRole("navigation").getByRole("button", { name: name === "Models" ? /^Models(?: \d+)?$/ : name, exact: name !== "Models" }).click();
}
async function policy(request: APIRequestContext, value: string) {
  const catalog = await (await request.get("/v1/juiced/catalog")).json();
  const model = catalog.data.find((model: { id: string }) => model.id === modelId);
  const response = await request.post("/v1/juiced/policy", { data: { model: modelId, policy: value, expected_revision: model.revision } });
  expect(response.ok()).toBe(true);
}

test.beforeEach(async ({ page, request }) => {
  await policy(request, "on-demand");
  expect((await request.post("/v1/unload", { data: { model: modelId } })).ok()).toBe(true);
  await page.goto("/");
  await expect(page.getByRole("button", { name: modelId, exact: true })).toBeVisible();
});

test("policy drafts, revision conflicts, lifecycle and keyboard dismissal", async ({ page, request }) => {
  await page.getByRole("button", { name: modelId, exact: true }).click();
  await page.getByLabel("Policy", { exact: true }).selectOption("hot");
  await navigate(page, "Diagnostics");
  await expect(page.getByRole("dialog")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).not.toBeVisible();
  await expect(page.getByLabel("Policy", { exact: true })).toHaveValue("hot");
  await page.getByRole("button", { name: "Save policy", exact: true }).click();
  await expect(page.getByRole("status")).toContainText("Saved Hot policy");
  await expect(page.getByRole("button", { name: "Save policy", exact: true })).toBeDisabled();
  await page.getByLabel("Policy", { exact: true }).selectOption("on-demand");
  await page.getByRole("button", { name: "Save policy", exact: true }).click();
  await expect(page.getByRole("status")).toContainText("Saved On demand policy");
  await page.getByRole("button", { name: "Load worker", exact: true }).click();
  await expect(page.getByRole("status")).toContainText(`Loaded ${modelId}`);
  await page.getByRole("button", { name: "Unload", exact: true }).click();
  await expect(page.getByRole("status")).toContainText(`Unloaded ${modelId}`);
  await page.getByLabel("Policy", { exact: true }).selectOption("hot");
  await policy(request, "disabled");
  await page.getByRole("button", { name: "Save policy", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText("changed in another client");
  await expect(page.getByLabel("Policy", { exact: true })).toHaveValue("hot");
  await page.getByRole("button", { name: "Cancel draft", exact: true }).click();
  await expect(page.getByLabel("Policy", { exact: true })).toHaveValue("disabled");
  await expect(page.getByRole("button", { name: "Load worker", exact: true })).toBeDisabled();
});

test("queued startup is visible and stopping the wait preserves saved policy", async ({ page, request }) => {
  const fixture = await (await request.get(`${sourceOrigin}/fixture.json`)).json();
  const suffix = randomUUID();
  const ids = [`fixture.blocked-${suffix}`, `fixture.queued-${suffix}`];
  const releases = ids.map((id) => join(dirname(fixture.model_file), `${id}.release`));
  for (const [index, id] of ids.entries()) {
    const args = [...fixture.args];
    args[5] = join(dirname(fixture.model_file), `${id}.events`);
    args.push("--parallel", "1", "--startup-release-file", releases[index]);
    const registered = await request.post("/v1/models/register", { data: {
      id, model_file: fixture.model_file, command: fixture.command, provider: "llama-cpp", args,
      resident_bytes: 64 * 1024 ** 2, max_inflight: 1, operations: ["chat/completions"], policy: "disabled", revision: 0,
    } });
    const registration = await registered.json();
    expect(registered.ok(), JSON.stringify(registration)).toBe(true);
    const enabled = await request.post("/v1/juiced/policy", { data: { model: id, policy: "on-demand", expected_revision: registration.revision } });
    expect(enabled.ok(), await enabled.text()).toBe(true);
  }
  const blocked = request.post("/v1/load", { data: { model: ids[0] } });
  void blocked.catch(() => undefined);
  const admission = async () => (await (await request.get("/v1/juiced/admission")).json()).startup;
  const model = async (id: string) => (await (await request.get("/v1/juiced/catalog")).json()).data.find((entry: { id: string }) => entry.id === id);
  const queuedRevision = (await model(ids[1]!)).revision;
  try {
    await expect.poll(async () => (await admission()).active_models).toContain(ids[0]);
    await page.getByRole("button", { name: "Refresh service state" }).click();
    await page.getByRole("button", { name: ids[1], exact: true }).click();
    await page.getByRole("button", { name: "Load worker", exact: true }).click();
    await expect.poll(async () => (await admission()).pending).toBe(1);
    await page.getByRole("button", { name: "Refresh service state" }).click();
    const inspector = page.getByRole("complementary", { name: "Model inspector" });
    await expect(inspector.getByText("Waiting to start", { exact: true })).toBeVisible();
    await expect(inspector.getByRole("button", { name: "Save policy", exact: true })).toBeDisabled();
    await expect(inspector.getByText(/no process or resident reservation yet/)).toBeVisible();
    await page.evaluate(() => { document.documentElement.style.fontSize = "32px"; });
    expect(await page.locator("main").evaluate((element) => element.scrollWidth <= element.clientWidth + 1)).toBe(true);
    await inspector.getByRole("button", { name: "Stop waiting", exact: true }).click();
    await expect(page.getByRole("status")).toContainText(`Stopped waiting for ${ids[1]}`);
    await expect.poll(async () => (await admission()).pending).toBe(0);
    await expect.poll(async () => (await model(ids[1]!)).preparation_pending).toBe(false);
    const cancelled = await model(ids[1]!);
    expect(cancelled.policy).toBe("on-demand");
    expect(cancelled.revision).toBe(queuedRevision);
    expect(cancelled.pid).toBeNull();
    expect(cancelled.loaded).toBe(false);
    expect((await admission()).active_models).toContain(ids[0]);
    await expect(inspector.getByRole("button", { name: "Stop waiting", exact: true })).toHaveCount(0);
    await inspector.getByLabel("Policy", { exact: true }).selectOption("disabled");
    await inspector.getByRole("button", { name: "Save policy", exact: true }).click();
    await expect(page.getByRole("status")).toContainText(`Saved Disabled policy for ${ids[1]}`);
  } finally {
    await Promise.all(releases.map((path) => writeFile(path, "release")));
    await blocked;
    for (const id of ids) {
      expect((await request.post("/v1/unload", { data: { model: id } })).ok()).toBe(true);
      const current = await model(id!);
      const disabled = await request.post("/v1/juiced/policy", { data: { model: id, policy: "disabled", expected_revision: current.revision } });
      expect(disabled.ok()).toBe(true);
      expect((await request.post("/v1/delete", { data: { model: id, expected_revision: (await disabled.json()).revision } })).ok()).toBe(true);
    }
    await Promise.all(releases.map((path) => rm(path, { force: true })));
  }
});

test("temporary service test, model change and metadata-only diagnostics", async ({ page }, info) => {
  info.annotations.push({ type: "scope", description: "GET request-limit projection with actual Rust inference forwarding; protected backend qualification is in rust-tests." });
  await page.route("**/v1/juiced/catalog", async (route) => {
    const response = await route.fetch();
    const catalog = await response.json();
    for (const model of catalog.data) if (model.id === modelId) model.request_limits = { input_bytes: 65536, batch: 1, output_tokens: 8 };
    await route.fulfill({ response, json: catalog });
  });
  const prompt = "Private UI fixture prompt that must not appear in diagnostics.";
  await page.getByRole("button", { name: "Refresh service state" }).click();
  await navigate(page, "Test services");
  await expect(page.getByRole("button", { name: "Run test", exact: true })).toBeDisabled();
  await page.getByLabel("Model", { exact: true }).selectOption(modelId);
  await page.getByLabel("Temporary prompt").fill(prompt);
  await expect(page.getByText("This test requests at most 8 output tokens.", { exact: true })).toBeVisible();
  const outgoing = page.waitForRequest((request) => request.method() === "POST" && new URL(request.url()).pathname === "/v1/chat/completions");
  await page.getByRole("button", { name: "Run test", exact: true }).click();
  expect((await outgoing).postDataJSON().max_tokens).toBe(8);
  await expect(page.locator(".test-output")).toContainText("Rust fixture reply");
  await page.getByLabel("Model", { exact: true }).selectOption("fixture.chat-small-q4");
  await expect(page.locator(".test-output")).not.toContainText("Rust fixture reply");
  await navigate(page, "Diagnostics");
  await page.getByRole("button", { name: "Refresh service state" }).click();
  await expect(page.locator(".request-list")).toContainText("completed");
  await expect(page.locator(".request-list")).not.toContainText(prompt);
  await navigate(page, "Test services");
  await expect(page.getByLabel("Temporary prompt")).toBeEmpty();
});

test("all destinations, narrow inspector, 200 percent text and bundled assets", async ({ page }, info) => {
  const failures: string[] = [];
  page.on("pageerror", (error) => failures.push(error.message));
  for (const name of ["Loading policy", "Library", "Backends & hardware", "Cache", "Service APIs", "Diagnostics", "Test services"]) {
    await navigate(page, name);
    await expect(page.getByRole("heading", { name, level: 1, exact: true })).toBeVisible();
    expect(await page.evaluate(() => document.body.scrollWidth <= innerWidth)).toBe(true);
    expect(await page.locator("main").evaluate((element) => element.scrollWidth <= element.clientWidth + 1), `${name} content fits its scroll container`).toBe(true);
  }
  await navigate(page, "Models");
  await page.getByRole("button", { name: modelId, exact: true }).click();
  await expect(page.getByRole("complementary", { name: "Model inspector" })).toBeVisible();
  const footerVisible = () => page.evaluate(() => document.querySelector(".inspector-footer")!.getBoundingClientRect().bottom <= document.querySelector("main")!.getBoundingClientRect().bottom);
  const closeInspector = async () => {
    const back = page.getByRole("button", { name: "Back to models", exact: true });
    if (await back.isVisible()) await back.click();
    else await page.getByRole("button", { name: "Close model inspector", exact: true }).click();
  };
  expect(await footerVisible()).toBe(true);
  await page.screenshot({ path: info.outputPath("inspector.png") });
  await page.evaluate(() => { document.documentElement.style.fontSize = "32px"; });
  expect(await page.evaluate(() => document.body.scrollWidth <= innerWidth)).toBe(true);
  expect(await footerVisible()).toBe(true);
  await page.screenshot({ path: info.outputPath("large-text.png") });
  await closeInspector();
  await page.getByRole("button", { name: modelId, exact: true }).click();
  await expect(page.getByRole("complementary", { name: "Model inspector" })).toBeVisible();
  expect(await footerVisible()).toBe(true);
  await closeInspector();
  for (const name of ["Loading policy", "Library", "Backends & hardware", "Cache", "Service APIs", "Diagnostics", "Test services"]) {
    await navigate(page, name);
    await expect(page.getByRole("heading", { name, level: 1, exact: true })).toBeVisible();
    expect(await page.locator("main").evaluate((element) => element.scrollWidth <= element.clientWidth + 1), `${name} large text fits its scroll container`).toBe(true);
  }
  expect(await page.evaluate(() => [...document.images].every((image) => image.complete && image.naturalWidth > 0))).toBe(true);
  expect(await page.evaluate(() => document.fonts.check("14px Inter"))).toBe(true);
  expect(failures).toEqual([]);
});

test("failed refresh retains inventory and disables mutations", async ({ page }) => {
  await page.getByRole("button", { name: modelId, exact: true }).click();
  await page.route("**/v1/juiced/catalog", (route) => route.fulfill({ status: 503, json: { error: { code: "offline", message: "Fixture service unavailable. Retry the connection." } } }));
  await page.getByRole("button", { name: "Refresh service state" }).click();
  await expect(page.getByRole("alert")).toContainText("Showing the last observation");
  await expect(page.getByRole("complementary", { name: "Model inspector" })).toContainText(modelId);
  await expect(page.getByLabel("Policy", { exact: true })).toBeDisabled();
  await expect(page.getByRole("button", { name: "Load worker", exact: true })).toBeDisabled();
});

test("retained unclaimed files stay visible without registration or deletion controls", async ({ page, request }) => {
  const filename = `job-${"e".repeat(32)}.part`;
  const inventory = await (await request.get("/v1/juiced/artifacts")).json();
  const retained = inventory.quarantined_files.find((file: { filename: string }) => file.filename === filename);
  expect(retained.reserved_bytes).toBeGreaterThan(0);
  expect(inventory.jobs.some((job: { id: string }) => job.id === "e".repeat(32))).toBe(false);
  await navigate(page, "Library");
  const observations = page.getByRole("region", { name: "Retained files awaiting inspection" });
  await expect(observations.getByText(filename, { exact: true })).toBeVisible();
  await expect(observations).toContainText("storage reservations remain charged");
  await expect(observations.getByRole("button")).toHaveCount(0);
  await page.evaluate(() => { document.documentElement.style.fontSize = "32px"; });
  expect(await page.locator("main").evaluate((element) => element.scrollWidth <= element.clientWidth + 1)).toBe(true);
});

test("read-only suitability retains unknowns, retries and invalidates on draft changes", async ({ page, request }, info) => {
  await page.getByRole("button", { name: modelId, exact: true }).click();
  const before = await (await request.get("/v1/juiced/state")).json();
  const catalog = await (await request.get("/v1/juiced/catalog")).json();
  await page.route("**/v1/juiced/suitability?*", (route) => route.fulfill({ status: 503, json: { error: { message: "Fixture check unavailable. Retry the check." } } }));
  await page.getByRole("button", { name: "Check saved settings", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText("Fixture check unavailable");
  await page.unroute("**/v1/juiced/suitability?*");
  await page.getByRole("button", { name: "Check saved settings", exact: true }).click();
  await expect(page.getByRole("region", { name: "Host fit" })).toContainText("Physical fit unknown");
  await page.getByRole("region", { name: "Host fit" }).scrollIntoViewIfNeeded();
  expect(await page.evaluate(() => document.body.scrollWidth <= innerWidth)).toBe(true);
  await page.screenshot({ path: info.outputPath("suitability.png") });
  const after = await (await request.get("/v1/juiced/state")).json();
  expect(after.resources).toEqual(before.resources);
  expect((await (await request.get("/v1/juiced/catalog")).json()).data).toEqual(catalog.data);
  await page.getByLabel("Policy", { exact: true }).selectOption("hot");
  await expect(page.getByRole("region", { name: "Host fit" })).not.toContainText("Physical fit unknown");
  await expect(page.getByRole("button", { name: "Check saved settings", exact: true })).toBeDisabled();
  await page.getByRole("button", { name: "Cancel draft", exact: true }).click();
  let started = () => {};
  const pending = new Promise<void>((resolve) => { started = resolve; });
  let release = () => {};
  const held = new Promise<void>((resolve) => { release = resolve; });
  await page.route("**/v1/juiced/suitability?*", async (route) => {
    const response = await route.fetch();
    started();
    await held;
    await route.fulfill({ response }).catch(() => {});
  });
  await page.getByRole("button", { name: "Check saved settings", exact: true }).click();
  await pending;
  await page.getByLabel("Policy", { exact: true }).selectOption("disabled");
  release();
  await expect(page.getByRole("region", { name: "Host fit" })).not.toContainText("Physical fit unknown");
  await expect(page.getByRole("button", { name: "Check saved settings", exact: true })).toBeDisabled();
});

type SourceFixture = { model_file: string; source_url: string; sha256: string; bytes: number; command: string; args: string[] };
async function sourceFixture(request: APIRequestContext): Promise<SourceFixture> {
  return (await request.get(`${sourceOrigin}/fixture.json`)).json();
}
async function acquire(page: Page, fixture: SourceFixture, url = fixture.source_url) {
  await page.getByLabel("Download URL", { exact: true }).fill(url);
  await page.getByLabel("Publisher SHA256").fill(fixture.sha256);
  await page.getByLabel("Expected size in bytes", { exact: true }).fill(String(fixture.bytes));
  await page.getByLabel("Storage ceiling in bytes", { exact: true }).fill(String(fixture.bytes));
  const response = page.waitForResponse((response) => response.url().endsWith("/v1/juiced/artifacts/pull") && response.request().method() === "POST");
  await page.getByRole("button", { name: "Start download", exact: true }).click();
  const result = await response;
  expect(result.ok()).toBe(true);
  const job = await result.json();
  await expect(page.getByRole("status")).toContainText("Download accepted");
  return page.getByRole("article", { name: `Download job ${job.id}`, exact: true });
}

test("managed acquisition, paused transfer, explicit registration and retained-weight removal", async ({ page, request }, info) => {
  const fixture = await sourceFixture(request);
  const registered = `browser.managed-${info.project.name}`;
  await navigate(page, "Library");
  await page.getByLabel("Download URL", { exact: true }).fill(fixture.source_url);
  await page.getByLabel("Publisher SHA256").fill("invalid");
  await page.getByRole("button", { name: "Start download", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText("SHA256");
  await expect(page.getByLabel("Download URL", { exact: true })).toHaveValue(fixture.source_url);
  const job = await acquire(page, fixture);
  await job.getByRole("button", { name: "Pause", exact: true }).click();
  await expect(job.locator(".badge")).toHaveText("paused");
  await job.getByRole("button", { name: "Resume", exact: true }).click();
  await expect(job.locator(".badge")).toHaveText("complete", { timeout: 10000 });
  const artifact = page.getByRole("article", { name: `Weight file ${fixture.sha256}`, exact: true });
  await expect(artifact).toBeVisible();
  await artifact.getByRole("button", { name: "Register model", exact: true }).click();
  const form = page.getByRole("form", { name: "Register managed model" });
  await page.getByLabel("New model ID", { exact: true }).fill(registered);
  await page.getByLabel("Engine executable", { exact: true }).fill(fixture.command);
  await page.getByLabel("Engine provider", { exact: true }).selectOption("whisper-cpp");
  await expect(page.getByLabel("Physical slots", { exact: true })).toHaveValue("1");
  await expect(page.getByLabel("Physical slots", { exact: true })).toBeDisabled();
  await expect(page.getByRole("checkbox", { name: "audio/transcriptions", exact: true })).toBeChecked();
  await expect(page.getByLabel("Engine arguments as JSON")).toHaveValue(/--no-gpu/);
  await page.getByLabel("Engine provider", { exact: true }).selectOption("llama-cpp");
  await page.getByLabel("Initial total context budget in tokens", { exact: true }).fill("1024");
  await page.getByText("Advanced engine arguments", { exact: true }).click();
  await page.getByLabel("Engine arguments as JSON").fill(JSON.stringify(fixture.args));
  await page.getByLabel("Resident reservation in bytes", { exact: true }).fill("1048576");
  await form.getByRole("button", { name: "Register Disabled model", exact: true }).click();
  await expect(form.getByRole("status")).toContainText(`Registered ${registered} as Disabled`);
  expect(await page.evaluate(() => document.body.scrollWidth <= innerWidth)).toBe(true);
  await page.screenshot({ path: info.outputPath("managed-library.png") });
  await expect(artifact.getByRole("button", { name: "Remove weights", exact: true })).toBeDisabled();
  const entry = page.getByRole("region", { name: "Registered library models" }).locator(".management-row").filter({ hasText: registered });
  await entry.getByRole("button", { name: "Manage model", exact: true }).click();
  await expect(page.getByRole("complementary", { name: "Model inspector" })).toContainText(registered);
  await page.getByRole("button", { name: "Remove registration", exact: true }).click();
  await expect(page.getByRole("status")).toContainText("Weight files retained");
  await navigate(page, "Library");
  const retained = page.getByRole("article", { name: `Weight file ${fixture.sha256}`, exact: true });
  await expect(retained.getByRole("button", { name: "Remove weights", exact: true })).toBeEnabled();
  await retained.getByRole("button", { name: "Remove weights", exact: true }).click();
  await expect(retained).toHaveCount(0);
});

test("failed download retries and cancellation settles without publishing a file", async ({ page, request }, info) => {
  const fixture = await sourceFixture(request);
  await navigate(page, "Library");
  const url = new URL(`/unstable/${info.project.name}.gguf`, fixture.source_url).href;
  const job = await acquire(page, fixture, url);
  await expect(job.locator(".badge")).toHaveText("failed");
  await expect(page.getByLabel("Download URL", { exact: true })).toHaveValue(url);
  await job.getByRole("button", { name: "Retry", exact: true }).click();
  await expect(job.locator(".badge")).toHaveText("complete", { timeout: 10000 });
  const artifact = page.getByRole("article", { name: `Weight file ${fixture.sha256}`, exact: true });
  await artifact.getByRole("button", { name: "Remove weights", exact: true }).click();
  await expect(artifact).toHaveCount(0);
  const cancelled = await acquire(page, fixture);
  await cancelled.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(cancelled.locator(".badge")).toHaveText("cancelled");
  await expect(page.getByRole("region", { name: "Available weight files" })).toContainText("No validated weight files");
});

test("explicit cache opt-in, session ownership, save, restore, inspect and forget", async ({ page, request }, info) => {
  await navigate(page, "Cache");
  await page.getByRole("button", { name: "Enable snapshots", exact: true }).click();
  await page.getByLabel("Snapshot model", { exact: true }).selectOption(modelId);
  await page.getByLabel("Opaque session ID", { exact: true }).fill("browser-private-context");
  await page.getByLabel("Temporary completion prompt").fill("Temporary browser pipeline context");
  await page.getByRole("button", { name: "Complete session request", exact: true }).click();
  await expect(page.getByRole("button", { name: "Save slot 0 snapshot", exact: true })).toBeEnabled();
  await page.getByRole("button", { name: "Save slot 0 snapshot", exact: true }).click();
  await expect(page.getByRole("status")).toContainText("Saved a validated snapshot");
  const snapshot = page.getByRole("region", { name: "Saved snapshots" }).getByRole("article");
  await expect(snapshot).toHaveCount(1);
  await snapshot.scrollIntoViewIfNeeded();
  expect(await page.evaluate(() => document.body.scrollWidth <= innerWidth)).toBe(true);
  await page.screenshot({ path: info.outputPath("saved-snapshot.png") });
  await page.getByLabel("Opaque session ID", { exact: true }).fill("wrong-context");
  await snapshot.getByRole("button", { name: "Restore snapshot", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText("snapshot");
  await expect(page.getByLabel("Opaque session ID", { exact: true })).toHaveValue("wrong-context");
  await page.getByLabel("Opaque session ID", { exact: true }).fill("browser-private-context");
  await snapshot.getByRole("button", { name: "Restore snapshot", exact: true }).click();
  await expect(page.getByRole("status")).toContainText("acknowledged restoration");
  await page.getByRole("button", { name: "Inspect retained storage", exact: true }).click();
  await expect(page.locator(".inspection-result")).toContainText("Checksum valid");
  await snapshot.getByRole("button", { name: "Forget snapshot", exact: true }).click();
  await expect(snapshot).toHaveCount(0);
  await page.getByRole("button", { name: "Complete session request", exact: true }).click();
  await expect(page.getByRole("button", { name: "Save slot 0 snapshot", exact: true })).toBeEnabled();
  await page.getByLabel("Snapshot ceiling in bytes", { exact: true }).fill("1");
  await page.getByRole("button", { name: "Save slot 0 snapshot", exact: true }).click();
  await expect(page.getByRole("alert")).toBeVisible();
  await expect(snapshot).toHaveCount(1);
  const recover = snapshot.getByRole("button", { name: "Recover retained reservation", exact: true });
  await recover.click();
  await expect(page.getByRole("alert")).toBeVisible();
  await expect(snapshot).toHaveCount(1);
  const unloaded = await request.post("/v1/unload", { data: { model: modelId } });
  expect(unloaded.ok()).toBe(true);
  await recover.click();
  await expect(page.getByRole("status")).toContainText("recovered the retained snapshot reservation");
  await expect(snapshot).toHaveCount(0);
  await page.getByRole("button", { name: "Disable snapshots", exact: true }).click();
  await expect(page.getByRole("button", { name: "Enable snapshots", exact: true })).toBeEnabled();
  expect(await page.evaluate(() => Object.values(localStorage).join(" "))).not.toContain("browser-private-context");
  const saved = await (await request.get("/v1/juiced/cache")).json();
  expect(saved.enabled).toBe(false);
  expect(saved.data).toHaveLength(0);
});

test("maintenance pause preserves policy and live API capabilities expose restrictions", async ({ page, request }) => {
  const before = await (await request.get("/v1/juiced/catalog")).json();
  await page.getByRole("button", { name: "Pause new requests", exact: true }).click();
  await expect(page.getByRole("region", { name: "Maintenance admission" })).toContainText("New requests paused");
  expect((await (await request.get("/v1/juiced/catalog")).json()).data).toEqual(before.data);
  await page.getByRole("button", { name: "Resume requests", exact: true }).click();
  await expect(page.getByRole("region", { name: "Maintenance admission" })).toContainText("Accepting new requests");
  await navigate(page, "Service APIs");
  const mcp = page.locator(".protocol-row").filter({ has: page.getByRole("heading", { name: "mcp", exact: true }) });
  await expect(mcp).toContainText("partial");
  await expect(mcp).toContainText("Stateless JSON");
  await page.getByText("Registered API endpoints", { exact: true }).click();
  await expect(page.getByText("POST /mcp", { exact: true })).toBeVisible();
  await expect(page.getByText("POST /v1/juiced/cache/save", { exact: true })).toBeVisible();
});

test("changing connection cancels stale cache inventory and clears private session drafts", async ({ page }) => {
  await navigate(page, "Cache");
  await page.getByLabel("Opaque session ID", { exact: true }).fill("tab-only-sensitive-context");
  let started = () => {}; const pending = new Promise<void>((resolve) => { started = resolve; });
  let release = () => {}; const held = new Promise<void>((resolve) => { release = resolve; });
  let first = true;
  await page.route("**/v1/juiced/cache*", async (route) => {
    if (!first || route.request().method() !== "GET") { await route.continue(); return; }
    first = false; const response = await route.fetch(); started(); await held;
    await route.fulfill({ response }).catch(() => {});
  });
  await page.getByRole("button", { name: "Refresh snapshots", exact: true }).click();
  await pending;
  await page.getByRole("button", { name: "Connection settings", exact: true }).first().click();
  await page.getByLabel("Service origin", { exact: true }).fill("http://127.0.0.1:13307");
  await page.getByLabel("API key", { exact: true }).fill("browser-memory-only-key");
  await page.getByRole("button", { name: "Connect", exact: true }).click();
  release();
  await expect(page.getByLabel("Opaque session ID", { exact: true })).toBeEmpty();
  expect(await page.evaluate(() => Object.values(localStorage).join(" "))).not.toContain("tab-only-sensitive-context");
  expect(await page.evaluate(() => Object.values(localStorage).join(" "))).not.toContain("browser-memory-only-key");
  await page.reload();
  await expect(page.getByLabel("Opaque session ID", { exact: true })).toBeEmpty();
  await page.getByRole("button", { name: "Connection settings", exact: true }).first().click();
  await expect(page.getByLabel("API key", { exact: true })).toBeEmpty();
});

test("typed launch drafts preserve revisions and policy while context comes from engine observations", async ({ page, request }, info) => {
  const fixture = await sourceFixture(request);
  const id = `browser.profile-${info.project.name}`;
  const path = `/v1/models/${encodeURIComponent(id)}/options`;
  const options = { context_tokens: 1024, threads: 2, gpu_layers: 0, parallel_slots: 2 };
  const registered = await request.post("/v1/models/register", { data: { id, model_file: fixture.model_file, provider: "llama-cpp", options, command: fixture.command, args: [...fixture.args, "--parallel", "2", "--ctx-size", "1024"], resident_bytes: 1048576, max_inflight: 2, operations: ["chat/completions", "completions"], policy: "disabled", revision: 0 } });
  expect(registered.ok()).toBe(true);
  await page.getByRole("button", { name: "Refresh service state" }).click();
  await page.getByRole("button", { name: id, exact: true }).click();
  const panel = page.getByRole("region", { name: "Context and launch profile" });
  const input = panel.getByLabel("Total context budget in tokens", { exact: true });
  await expect(input).toHaveValue("1024");
  await expect(panel).toContainText("2 · fixed registration");
  await expect(panel).toContainText("Worker unloaded. Serving context is unknown.");
  await input.fill("2048");
  await navigate(page, "Diagnostics");
  await expect(page.getByRole("dialog")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(input).toHaveValue("2048");
  const before = await (await request.get(path)).json();
  expect((await request.post(path, { data: { expected_revision: before.revision, options: { ...options, context_tokens: 3072, threads: 3 } } })).ok()).toBe(true);
  await panel.getByRole("button", { name: "Save launch profile", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText("profile changed");
  await expect(input).toHaveValue("2048");
  await panel.getByRole("button", { name: "Cancel launch draft", exact: true }).click();
  await expect(input).toHaveValue("3072");
  await input.fill("4096");
  await panel.getByLabel("Threads", { exact: true }).fill("");
  await panel.getByRole("button", { name: "Save launch profile", exact: true }).click();
  await expect(page.getByRole("status")).toContainText("Policy preserved; worker remains unloaded");
  const saved = await (await request.get(path)).json();
  expect(saved.saved).toEqual({ context_tokens: 4096, threads: null, gpu_layers: 0, parallel_slots: 2 });
  const catalog = await (await request.get("/v1/juiced/catalog")).json();
  expect(catalog.data.find((model: { id: string }) => model.id === id)).toMatchObject({ policy: "disabled", loaded: false });
  await page.getByLabel("Policy", { exact: true }).selectOption("on-demand");
  await page.getByRole("button", { name: "Save policy", exact: true }).click();
  await page.getByRole("button", { name: "Load worker", exact: true }).click();
  await expect(page.getByRole("status")).toContainText(`Loaded ${id}`);
  await expect(input).toBeDisabled();
  const observed = panel.locator(".context-observation .facts > div").filter({ hasText: "Observed request context" }).locator("dd");
  await expect(observed).toHaveText("4,096 tokens");
  await expect(panel.locator(".slot-observation")).toHaveCount(2);
  await expect(panel.locator(".slot-observation").first()).toContainText("4,096 tokens");
  await panel.locator(".context-observation").scrollIntoViewIfNeeded();
  expect(await page.evaluate(() => document.body.scrollWidth <= innerWidth)).toBe(true);
  await page.screenshot({ path: info.outputPath("context-profile.png") });
  await page.getByRole("button", { name: "Unload", exact: true }).click();
  await page.getByLabel("Policy", { exact: true }).selectOption("disabled");
  await page.getByRole("button", { name: "Save policy", exact: true }).click();
  await page.getByRole("button", { name: "Remove registration", exact: true }).click();
  await expect(page.getByRole("status")).toContainText("Weight files retained");
});

test("native settings capability projection edits and clears only context through the real options endpoint", async ({ page, request }, info) => {
  info.annotations.push({ type: "scope", description: "GET capability projection over a local fixture registration; POSTs use the actual Rust options owner. This does not qualify native settings import." });
  const fixture = await sourceFixture(request);
  const id = `browser.native-context-${info.project.name}-${randomUUID()}`;
  const path = `/v1/models/${encodeURIComponent(id)}/options`;
  const registered = await request.post("/v1/models/register", { data: {
    id, model_file: fixture.model_file, provider: "llama-cpp",
    options: { context_tokens: 1024, threads: 2, gpu_layers: 0, parallel_slots: 2 },
    command: fixture.command, args: [...fixture.args, "--parallel", "2", "--ctx-size", "1024"],
    resident_bytes: 1048576, max_inflight: 2, operations: ["chat/completions", "completions"], policy: "disabled", revision: 0,
  } });
  expect(registered.ok(), await registered.text()).toBe(true);
  const original = await (await request.get(path)).json();
  const writes: unknown[] = [];
  page.on("request", (outgoing) => {
    if (outgoing.method() === "POST" && new URL(outgoing.url()).pathname === path) writes.push(outgoing.postDataJSON());
  });
  await page.route(`**${path}`, async (route) => {
    if (route.request().method() !== "GET") { await route.continue(); return; }
    const response = await route.fetch();
    if (!response.ok()) { await route.fulfill({ response }); return; }
    const options = await response.json();
    await route.fulfill({ response, json: {
      ...options, options_authority: "native-settings", editable_fields: ["context_tokens"],
      saved: { context_tokens: options.saved.context_tokens, threads: null, gpu_layers: null, parallel_slots: null },
    } });
  });
  await page.getByRole("button", { name: "Refresh service state" }).click();
  await page.getByRole("button", { name: id, exact: true }).click();
  const panel = page.getByRole("region", { name: "Context and launch profile" });
  const input = panel.getByLabel("Total context budget in tokens", { exact: true });
  const save = panel.getByRole("button", { name: "Save context override", exact: true });
  const cancel = panel.getByRole("button", { name: "Cancel context draft", exact: true });
  await expect(input).toHaveValue("1024");
  await expect(panel.getByLabel("Threads", { exact: true })).toHaveCount(0);
  await expect(panel.getByLabel("GPU layers", { exact: true })).toHaveCount(0);
  await expect(panel).toContainText("Empty removes the override and uses the registered model setting");
  await expect(panel).toContainText("2 · fixed registration");
  await expect(save).toBeDisabled();
  await input.fill("2048");
  const editedResponse = page.waitForResponse((response) => response.request().method() === "POST" && new URL(response.url()).pathname === path);
  await save.click();
  expect((await editedResponse).ok()).toBe(true);
  await expect(save).toBeDisabled();
  await expect(cancel).toBeDisabled();
  await expect(input).toHaveValue("2048");
  expect(writes).toEqual([{ expected_revision: original.revision, options: { context_tokens: 2048, threads: null, gpu_layers: null, parallel_slots: null } }]);
  const edited = await (await request.get(path)).json();
  expect(edited.revision).toBeGreaterThan(original.revision);
  expect(edited.saved).toEqual({ context_tokens: 2048, threads: null, gpu_layers: null, parallel_slots: null });
  await input.fill("4096");
  await cancel.click();
  await expect(input).toHaveValue("2048");
  await expect(cancel).toBeDisabled();
  expect(writes).toHaveLength(1);
  await input.fill("");
  const clearedResponse = page.waitForResponse((response) => response.request().method() === "POST" && new URL(response.url()).pathname === path);
  await save.click();
  expect((await clearedResponse).ok()).toBe(true);
  await expect(save).toBeDisabled();
  await expect(cancel).toBeDisabled();
  await expect(input).toBeEmpty();
  expect(writes).toEqual([
    { expected_revision: original.revision, options: { context_tokens: 2048, threads: null, gpu_layers: null, parallel_slots: null } },
    { expected_revision: edited.revision, options: { context_tokens: null, threads: null, gpu_layers: null, parallel_slots: null } },
  ]);
  const cleared = await (await request.get(path)).json();
  expect(cleared.revision).toBeGreaterThan(edited.revision);
  expect(cleared.saved).toEqual({ context_tokens: null, threads: null, gpu_layers: null, parallel_slots: null });
  expect(cleared.effective_launch.context_tokens).toBe(1024);
  await expect(panel.locator(".facts > div").filter({ hasText: "Effective launch budget" }).locator("dd")).toHaveText("1,024 tokens");
  const catalog = await (await request.get("/v1/juiced/catalog")).json();
  expect(catalog.data.find((model: { id: string }) => model.id === id)).toMatchObject({ policy: "disabled", loaded: false, revision: cleared.revision });
  await expect(panel).toContainText("Worker unloaded. Serving context is unknown.");
  await page.getByRole("button", { name: "Remove registration", exact: true }).click();
  await expect(page.getByRole("status")).toContainText("Weight files retained");
});

test("runtime planning catalog preserves origins, native restrictions and stale-query boundaries", async ({ page }, info) => {
  await page.route("**/v1/juiced/runtimes", (route) => route.fulfill({ status: 503, json: { error: { code: "catalog_unavailable", message: "Fixture catalog unavailable. Retry the catalog." } } }));
  await navigate(page, "Backends & hardware");
  const catalog = page.getByRole("region", { name: "Runtime catalog", exact: true });
  await expect(catalog.getByRole("alert")).toContainText("Fixture catalog unavailable");
  await page.unroute("**/v1/juiced/runtimes");
  await catalog.getByRole("button", { name: "Refresh catalog", exact: true }).click();
  await expect(catalog).toContainText("69 definitions · 18 original recipes · 44 native declaration rows");
  await expect(catalog.getByRole("article")).toHaveCount(19);
  await expect(catalog.getByLabel("Delivery treatment", { exact: true })).toHaveValue("parity");
  await catalog.getByLabel("Search runtime definitions", { exact: true }).fill("transcribe");
  await expect(catalog.getByRole("article")).toHaveCount(1);
  await catalog.getByRole("button", { name: "FUTO transcribe.cpp ASR4ALL worker", exact: true }).click();
  await expect(catalog.getByRole("link", { name: "https://gitlab.futo.org/keyboard/transcribe.cpp", exact: true })).toBeVisible();
  await expect(catalog.locator(".runtime-detail")).toContainText("Native worker forces CPU and ASR4ALL architecture");
  await expect(catalog.locator(".runtime-detail")).toContainText("Not established");
  await catalog.getByLabel("Delivery treatment", { exact: true }).selectOption("conditional");
  await expect(catalog.getByRole("article")).toHaveCount(1);
  await catalog.getByRole("button", { name: "Handy transcribe.cpp", exact: true }).click();
  await expect(catalog.getByRole("link", { name: "https://github.com/handy-computer/transcribe.cpp", exact: true })).toBeVisible();
  await expect(catalog.locator(".runtime-detail")).toContainText("Separate origin from FUTO");
  await catalog.getByLabel("Search runtime definitions", { exact: true }).fill("");
  await catalog.getByLabel("Delivery treatment", { exact: true }).selectOption("first-expansion");
  await expect(catalog.getByRole("article")).toHaveCount(12);
  await expect(catalog.getByRole("button", { name: /^(Install|Enable|Load)/ })).toHaveCount(0);
  await catalog.getByText("Resolve a native recipe", { exact: true }).click();
  await catalog.getByLabel("Native backend label", { exact: true }).selectOption("hybrid-rocm");
  const resolvePattern = "**/v1/juiced/runtimes/resolve?*";
  await page.route(resolvePattern, (route) => route.fulfill({ status: 503, json: { error: { code: "mapping_unavailable", message: "Fixture mapping unavailable. Retry the mapping." } } }));
  await catalog.getByRole("button", { name: "Resolve mapping", exact: true }).click();
  const resolution = catalog.getByRole("region", { name: "Native recipe resolution", exact: true });
  await expect(resolution.getByRole("alert")).toContainText("Fixture mapping unavailable");
  await expect(catalog.getByLabel("Native backend label", { exact: true })).toHaveValue("hybrid-rocm");
  await page.unroute(resolvePattern);
  await resolution.getByRole("button", { name: "Retry mapping", exact: true }).click();
  await expect(resolution.getByRole("heading", { name: "llama-xdna-hybrid", exact: true })).toBeVisible();
  await expect(resolution).toContainText("explicit NPU/GPU placement");
  await expect(resolution.locator(".native-row")).toContainText("experimental");
  await expect(resolution.locator(".native-row")).toContainText("linux");
  await expect(resolution.locator(".native-row")).toContainText("gfx1150");
  await catalog.getByLabel("Native backend label", { exact: true }).selectOption("system");
  await expect(resolution).toHaveCount(0);
  await catalog.getByRole("button", { name: "Resolve mapping", exact: true }).click();
  await expect(resolution).toContainText("system names installation provenance");
  await expect(resolution).toContainText("Unresolved");
  await catalog.getByLabel("Native backend label", { exact: true }).selectOption("rocm");
  await catalog.getByRole("button", { name: "Resolve mapping", exact: true }).click();
  await expect(resolution.locator(".facts > div").filter({ hasText: "Compute vocabulary" }).locator("dd")).toHaveText("hip");
  await catalog.getByLabel("Native recipe", { exact: true }).selectOption("asr4all");
  await catalog.getByRole("button", { name: "Resolve mapping", exact: true }).click();
  await expect(resolution.getByRole("heading", { name: "futo-transcribe-cpp", exact: true })).toBeVisible();
  await expect(resolution.locator(".native-row")).toContainText("cpu (x86_64)");
  let started = () => {}; const pending = new Promise<void>((resolve) => { started = resolve; });
  let release = () => {}; const held = new Promise<void>((resolve) => { release = resolve; });
  let finish = () => {}; const finished = new Promise<void>((resolve) => { finish = resolve; });
  await page.route(resolvePattern, async (route) => { const response = await route.fetch(); started(); await held; await route.fulfill({ response }).catch(() => {}); finish(); });
  await catalog.getByLabel("Native recipe", { exact: true }).selectOption("llamacpp");
  await catalog.getByLabel("Native backend label", { exact: true }).selectOption("hybrid-rocm");
  await catalog.getByRole("button", { name: "Resolve mapping", exact: true }).click();
  await pending;
  await catalog.getByLabel("Native recipe", { exact: true }).selectOption("flm");
  await expect(resolution).toHaveCount(0);
  release(); await finished;
  await expect(resolution).toHaveCount(0);
  await page.unroute(resolvePattern);
  await catalog.getByRole("button", { name: "Resolve mapping", exact: true }).click();
  await expect(resolution.getByRole("heading", { name: "fastflowlm", exact: true })).toBeVisible();
  await expect(resolution).toContainText("npu names hardware");
  await expect(resolution).toContainText("Unresolved");
  await resolution.scrollIntoViewIfNeeded();
  expect(await page.evaluate(() => document.body.scrollWidth <= innerWidth)).toBe(true);
  await page.screenshot({ path: info.outputPath("runtime-catalog.png") });
});


test("navigation restores the page heading after browsing a long destination", async ({ page }) => {
  await navigate(page, "Backends & hardware");
  await expect(page.getByRole("heading", { name: "Definitions", exact: true })).toBeVisible();
  await page.locator("#workspace").evaluate((element) => { element.scrollTop = element.scrollHeight; });
  expect(await page.locator("#workspace").evaluate((element) => element.scrollTop)).toBeGreaterThan(0);
  await navigate(page, "Library");
  await expect(page.getByRole("heading", { name: "Library", level: 1, exact: true })).toBeInViewport();
  expect(await page.locator("#workspace").evaluate((element) => element.scrollTop)).toBe(0);
});

test("temporary transcription sends unchanged WAV bytes and supports retry, reset and cancellation", async ({ page, request }, info) => {
  const audio = await (await request.get(`${sourceOrigin}/speech-fixture.wav`)).body();
  await navigate(page, "Test services");
  await page.getByRole("group", { name: "Test operation" }).getByRole("button", { name: "Transcription", exact: true }).click();
  const panel = page.getByRole("region", { name: "Temporary transcription test", exact: true });
  await expect(panel.getByRole("button", { name: "Transcribe audio", exact: true })).toBeDisabled();
  await panel.getByLabel("Speech model", { exact: true }).selectOption("fixture.speech-tiny");
  await panel.getByLabel("Temporary WAV file", { exact: true }).setInputFiles({ name: "invalid.wav", mimeType: "audio/wav", buffer: Buffer.from("invalid content") });
  await panel.getByRole("button", { name: "Transcribe audio", exact: true }).click();
  await expect(panel.getByRole("alert")).toContainText("WAV");
  await expect(panel.locator(".selected-audio")).toContainText("invalid.wav");
  await panel.getByLabel("Temporary WAV file", { exact: true }).setInputFiles({ name: "existing.wav", mimeType: "audio/wav", buffer: audio });
  await panel.getByLabel("Audio language", { exact: true }).fill("de");
  const endpoint = "**/v1/audio/transcriptions";
  await page.route(endpoint, (route) => route.fulfill({ status: 503, json: { error: { code: "speech_unavailable", message: "Fixture speech worker unavailable. Retry the request." } } }));
  await panel.getByRole("button", { name: "Transcribe audio", exact: true }).click();
  await expect(panel.getByRole("alert")).toContainText("Fixture speech worker unavailable");
  await expect(panel.getByLabel("Audio language", { exact: true })).toHaveValue("de");
  await expect(panel.locator(".selected-audio")).toContainText("existing.wav");
  await page.unroute(endpoint);
  let capture!: (response: { ok: boolean; body: unknown }) => void;
  let rejectCapture!: (error: unknown) => void;
  const completed = new Promise<{ ok: boolean; body: unknown }>((resolve, reject) => { capture = resolve; rejectCapture = reject; });
  const captureSpeech = async (route: Route) => {
    try {
      const response = await route.fetch();
      const body: unknown = await response.json();
      await route.fulfill({ response });
      capture({ ok: response.ok(), body });
    } catch (error) { rejectCapture(error); await route.abort().catch(() => {}); }
  };
  await page.route(endpoint, captureSpeech);
  await panel.getByRole("button", { name: "Transcribe audio", exact: true }).click();
  const response = await completed; expect(response.ok).toBe(true);
  await page.unroute(endpoint, captureSpeech);
  expect(response.body).toMatchObject({
    received_file: { bytes: audio.byteLength, sha256: "1f010435b3ed4a2776cc1739ae5b0e524c951c9312c2a4922893b00a15bde633", filename: "existing.wav", content_type: "audio/wav" },
    received_fields: { language: "de", response_format: "json" }, received_model: false, received_authorization: false,
  });
  await expect(panel.locator(".transcription-output")).toContainText("Fixture speech λ");
  expect(await page.evaluate(() => document.body.scrollWidth <= innerWidth)).toBe(true);
  await page.screenshot({ path: info.outputPath("temporary-transcription.png") });
  await panel.getByLabel("Audio language", { exact: true }).fill("auto");
  await expect(panel.locator(".transcription-output")).not.toContainText("Fixture speech λ");
  const sent = page.waitForRequest((request) => request.url().endsWith("/v1/audio/transcriptions") && request.method() === "POST");
  const aborted = page.waitForEvent("requestfailed", { predicate: (request) => request.url().endsWith("/v1/audio/transcriptions") });
  await panel.getByRole("button", { name: "Transcribe audio", exact: true }).click(); await sent;
  await panel.getByRole("button", { name: "Stop test", exact: true }).click(); await aborted;
  await expect(panel.getByRole("alert")).toContainText("Stopped waiting");
  await expect(panel.locator(".transcription-output")).not.toContainText("Fixture speech λ");
  await panel.getByRole("button", { name: "Reset transcription", exact: true }).click();
  await expect(panel.getByLabel("Temporary WAV file", { exact: true })).toBeEmpty();
  await expect(panel.getByRole("button", { name: "Transcribe audio", exact: true })).toBeDisabled();
  await navigate(page, "Diagnostics");
  await page.getByRole("button", { name: "Refresh service state" }).click();
  await expect(page.locator(".request-list")).toContainText("audio/transcriptions");
  await expect(page.locator(".request-list")).not.toContainText("Fixture speech λ");
  await expect(page.locator(".request-list")).not.toContainText("existing.wav");
  await navigate(page, "Test services");
  await page.getByRole("button", { name: "Transcription", exact: true }).click();
  await expect(page.getByLabel("Temporary WAV file", { exact: true })).toBeEmpty();
  expect(await page.evaluate(() => Object.values(localStorage).join(" "))).not.toContain("existing.wav");
});

test("diagnostics separates persistent cache and runtime metadata from worker reservations and unavailable cache capacity", async ({ page }) => {
  let unavailable = false;
  await page.route("**/v1/juiced/state", async (route) => {
    const response = await route.fetch();
    const state = await response.json();
    state.resources.cache_control_memory = unavailable
      ? { state:"unavailable", reason:"unknown_host_capacity", metadata_bytes:0, scratch_bytes:0, reserved_bytes:0, outstanding_bytes:0, active_operations:0 }
      : { state:"accounted", metadata_bytes:64 * 1024 ** 2, scratch_bytes:0, reserved_bytes:64 * 1024 ** 2, outstanding_bytes:64 * 1024 ** 2, active_operations:0, bounds:{ bootstrap_bytes:128 * 1024 ** 2, metadata_bytes:64 * 1024 ** 2, scratch_bytes:66 * 1024 ** 2, state_scratch_bytes:22 * 1024 ** 2, page_scratch_bytes:8 * 1024 ** 2, response_bytes:32 * 1024 ** 2 } };
    state.resources.resident_budget_bytes = unavailable ? null : 256 * 1024 ** 2;
    state.resources.runtime_control_memory = { state:"accounted", metadata_bytes:32 * 1024 ** 2, binding_bytes:2 * 1024 ** 2, binding_count:1, scratch_bytes:0, active_operations:0, reserved_bytes:34 * 1024 ** 2, outstanding_bytes:34 * 1024 ** 2, bounds:{ metadata_bytes:32 * 1024 ** 2, bootstrap_bytes:48 * 1024 ** 2, import_bytes:64 * 1024 ** 2, response_work_bytes:8 * 1024 ** 2, response_bytes:4 * 1024 ** 2, binding_work_bytes:24 * 1024 ** 2 } };
    state.resources.reserved_bytes = (unavailable ? 0 : 64 * 1024 ** 2) + 34 * 1024 ** 2;
    await route.fulfill({ response, json:state });
  });
  await page.getByRole("button", { name:"Refresh service state" }).click();
  await navigate(page, "Diagnostics");
  const metadata = page.locator(".facts > div").filter({ has:page.getByText("Cache metadata reservation", { exact:true }) });
  await expect(metadata.locator("dd")).toHaveText("64.0 MiB");
  const work = page.locator(".facts > div").filter({ has:page.getByText("Cache work and responses", { exact:true }) });
  await expect(work.locator("dd")).toHaveText("0 B");
  const runtimeMetadata = page.locator(".facts > div").filter({ has:page.getByText("Runtime metadata reservation", { exact:true }) });
  const runtimeBindings = page.locator(".facts > div").filter({ has:page.getByText("Retained runtime bindings", { exact:true }) });
  await expect(runtimeMetadata.locator("dd")).toHaveText("32.0 MiB");
  await expect(runtimeBindings.locator("dd")).toHaveText("2.0 MiB");
  await expect(page.getByText(/Runtime package metadata and retained bindings share Reserved/)).toBeVisible();
  await expect(page.getByText(/Unloading a worker preserves that reservation/)).toBeVisible();
  await page.evaluate(() => { document.documentElement.style.fontSize = "32px"; });
  for (const field of [metadata, work, runtimeMetadata, runtimeBindings]) expect(await field.locator("dd").evaluate(element => { const range = document.createRange(); range.selectNodeContents(element); return range.getClientRects().length; })).toBe(1);
  expect(await page.locator("main").evaluate(element => element.scrollWidth <= element.clientWidth + 1)).toBe(true);
  unavailable = true;
  await page.getByRole("button", { name:"Refresh service state" }).click();
  await expect(page.getByText(/Cache metadata could not be admitted and remains unopened/)).toBeVisible();
  await expect(metadata).toHaveCount(0);
  await expect(runtimeMetadata.locator("dd")).toHaveText("32.0 MiB");
  await expect(page.locator(".global-footer").getByText("Service online", { exact:true })).toBeVisible();
  await page.route("**/v1/juiced/cache*", route => route.fulfill({ status:503, contentType:"application/json", body:JSON.stringify({ error:{ code:"unknown_host_capacity", message:"Protected capacity could not admit cache metadata. The cache remains unopened." } }) }));
  await navigate(page, "Cache");
  await expect(page.getByRole("button", { name:"Snapshot policy unavailable", exact:true })).toBeDisabled();
  const policy = page.locator(".management-section").filter({ has:page.getByRole("heading", { name:"Persistent snapshot policy", exact:true }) });
  await expect(policy.locator(".badge")).toHaveText("Unavailable");
  await expect(page.getByRole("heading", { name:"No snapshots in this credential context", exact:true })).toHaveCount(0);
  expect(await page.locator("main").evaluate(element => element.scrollWidth <= element.clientWidth + 1)).toBe(true);
});

test("cache pages bind their source, preserve private drafts and refuse stale continuations", async ({ page }) => {
  const row = (index: number) => ({ id:index.toString(16).padStart(32, "0"), revision:2, state:"saved", reserved_bytes:64, bytes:64, protocol:"openai.completions", session_digest:"c".repeat(64), active_readers:0, retained_readers:[] });
  const first = Array.from({ length:128 }, (_, index) => row(index + 1));
  const last = row(129);
  let changed = false;
  let hold = true;
  let releaseNext!: () => void; const waiting = new Promise<void>(resolve => { releaseNext = resolve; });
  let nextStarted!: () => void; const started = new Promise<void>(resolve => { nextStarted = resolve; });
  const requests: string[] = [];
  const view = () => changed ? "b".repeat(64) : "a".repeat(64);
  const generation = () => changed ? 8 : 7;
  await page.route("**/v1/juiced/cache*", async route => {
    const query = new URL(route.request().url()).searchParams;
    requests.push(route.request().url());
    expect(query.get("limit")).toBe("128");
    const after = query.get("after");
    if (after && hold) { nextStarted(); await waiting; }
    if (after && (query.get("expected_generation") !== String(generation()) || query.get("expected_view") !== view())) {
      await route.fulfill({ status:409, json:{ error:{ code:"cache_revision_conflict", message:"The cache changed. Refresh from the first page." } } }); return;
    }
    if (after) expect(after).toBe(first.at(-1)!.id);
    await route.fulfill({ json:{ schema_version:1, enabled:true, configuration_generation:1, generation:generation(), qualification:"unqualified", data:after ? [last] : first, page:{ view:view(), limit:128, next_after:after ? null : first.at(-1)!.id } } });
  });
  await page.route("**/v1/juiced/cache/inspect*", async route => {
    const query = new URL(route.request().url()).searchParams;
    const after = query.get("after");
    if (after) { expect(query.get("expected_generation")).toBe("8"); expect(query.get("expected_view")).toBe(view()); }
    await route.fulfill({ json:{ generation:generation(), read_only:true, recovery_authorized:false, charged_bytes:129 * 64, snapshots:(after ? [last] : first).map(snapshot => ({ snapshot, issues:[], checksum_valid:true })), orphan_files:["kv-observed.bin"], issues:[], page:{ view:view(), limit:128, next_after:after ? null : first.at(-1)!.id }, orphan_page:{ limit:128, next_after:null, observed_count:200, scan_complete:false } } });
  });
  await navigate(page, "Cache");
  const snapshots = page.getByRole("region", { name:"Saved snapshots", exact:true });
  const initial = snapshots.getByRole("article", { name:`Snapshot ${first[0]!.id}`, exact:true });
  await expect(initial).toBeVisible();
  await page.getByLabel("Opaque session ID", { exact:true }).fill("private-page-session");
  await snapshots.getByRole("button", { name:"Next snapshot page", exact:true }).click();
  await started;
  await expect(initial).toHaveCount(0);
  await expect(snapshots.getByRole("button", { name:"Next snapshot page", exact:true })).toBeDisabled();
  releaseNext(); hold = false;
  await expect(snapshots.getByRole("article", { name:`Snapshot ${last.id}`, exact:true })).toBeVisible();
  await expect(snapshots.getByText("Page 2", { exact:true })).toBeVisible();
  await expect(page.getByLabel("Opaque session ID", { exact:true })).toHaveValue("private-page-session");
  await snapshots.getByRole("button", { name:"Previous snapshot page", exact:true }).click();
  await expect(initial).toBeVisible();
  changed = true;
  await snapshots.getByRole("button", { name:"Next snapshot page", exact:true }).click();
  await expect(page.getByRole("alert")).toContainText("The cache changed");
  await expect(snapshots.getByText(/This snapshot page is unavailable/)).toBeVisible();
  await snapshots.getByRole("button", { name:"Restart snapshot pages", exact:true }).click();
  await expect(initial).toBeVisible();
  await expect(snapshots.getByText("Page 1", { exact:true })).toBeVisible();
  await expect(page.getByRole("alert")).toHaveCount(0);
  await page.getByRole("button", { name:"Inspect retained storage", exact:true }).click();
  const inspection = page.getByRole("region", { name:"Retained cache storage", exact:true });
  await expect(inspection.getByText(/200 observed untracked files/)).toBeVisible();
  await expect(inspection.getByText(/observed count is not a total/)).toBeVisible();
  await inspection.getByRole("button", { name:"Next inspection page", exact:true }).click();
  await expect(inspection.getByText("Page 2", { exact:true })).toBeVisible();
  await expect(inspection.getByText(last.id, { exact:true })).toBeVisible();
  await expect(page.getByLabel("Opaque session ID", { exact:true })).toHaveValue("private-page-session");
  expect(requests.every(url => !url.includes("private-page-session"))).toBe(true);
  await page.evaluate(() => { document.documentElement.style.fontSize = "32px"; });
  expect(await page.locator("main").evaluate(element => element.scrollWidth <= element.clientWidth + 1)).toBe(true);
});

test("failed resource polling backs off and a manual snapshot refresh remains immediate", async ({ page }) => {
  await page.clock.install();
  let calls = 0; let healthy = false;
  await page.route("**/v1/juiced/cache*", async route => {
    calls++;
    if (!healthy) await route.fulfill({ status:503, json:{ error:{ code:"unknown_host_capacity", message:"Cache capacity unavailable." } } });
    else await route.fulfill({ json:{ schema_version:1, enabled:true, configuration_generation:1, generation:0, qualification:"unqualified", data:[], page:{ limit:128, view:"a".repeat(64), next_after:null } } });
  });
  await navigate(page, "Cache");
  await expect(page.getByRole("alert")).toContainText("Cache capacity unavailable");
  const refresh = page.getByRole("button", { name:"Refresh snapshots", exact:true });
  await expect(refresh).toBeEnabled();
  expect(calls).toBe(1);
  await page.clock.runFor(2000);
  expect(calls).toBe(1);
  await page.clock.runFor(2000);
  await expect.poll(() => calls).toBe(2);
  await expect(refresh).toBeEnabled();
  await page.clock.runFor(4000);
  expect(calls).toBe(2);
  await refresh.click();
  await expect.poll(() => calls).toBe(3);
  await expect(refresh).toBeEnabled();
  healthy = true;
  await refresh.click();
  await expect(page.getByRole("alert")).toHaveCount(0);
  expect(calls).toBe(4);
  await expect(page.getByRole("button", { name:"Disable snapshots", exact:true })).toBeEnabled();
});

test("artifact sets retain member files through explicit registration and descriptor removal", async ({ page, request }, info) => {
  const fixture = await sourceFixture(request);
  const registered = `browser.set-${info.project.name}`;
  await navigate(page, "Library");
  const job = await acquire(page, fixture);
  await expect(job.locator(".badge")).toHaveText("complete", { timeout: 10000 });
  const artifact = page.getByRole("article", { name: `Weight file ${fixture.sha256}`, exact: true });
  await artifact.getByRole("button", { name: "Create model set", exact: true }).click();
  const form = page.getByRole("form", { name: "Register managed model" });
  await expect(form).toContainText("Artifact set: set-");
  await expect(page.getByLabel("New model ID", { exact: true })).toBeFocused();
  await page.getByLabel("New model ID", { exact: true }).fill(registered);
  await page.getByLabel("Engine executable", { exact: true }).fill(fixture.command);
  await page.getByLabel("Engine arguments as JSON").fill(JSON.stringify(fixture.args));
  await page.getByLabel("Resident reservation in bytes", { exact: true }).fill("1048576");
  await form.getByRole("button", { name: "Register Disabled model", exact: true }).click();
  await expect(form.getByRole("status")).toContainText(`Registered ${registered} as Disabled`);
  const inventory = await (await request.get("/v1/juiced/artifacts")).json();
  const record = inventory.artifact_sets.find((set: any) => set.specification.origin.importer_id === "juiced-console" && set.specification.members[0].artifact === fixture.sha256);
  expect(record).toBeTruthy();
  const set = page.getByRole("article", { name: `Artifact set ${record.id}`, exact: true });
  await expect(set).toContainText(`model:${registered}`);
  await expect(set.getByRole("button", { name: "Remove set", exact: true })).toBeDisabled();
  await expect(artifact.getByRole("button", { name: "Remove weights", exact: true })).toBeDisabled();
  await form.getByRole("button", { name: "Close registration", exact: true }).click();
  await set.getByRole("button", { name: "View member files", exact: true }).click();
  await expect(set.getByLabel("Set member files")).toContainText(fixture.sha256);
  await page.evaluate(() => { document.documentElement.style.fontSize = "32px"; });
  expect(await page.evaluate(() => document.body.scrollWidth <= innerWidth)).toBe(true);
  await page.screenshot({ path: info.outputPath("artifact-sets.png") });
  await page.evaluate(() => { document.documentElement.style.fontSize = ""; });
  const entry = page.getByRole("region", { name: "Registered library models" }).locator(".management-row").filter({ hasText: registered });
  await entry.getByRole("button", { name: "Manage model", exact: true }).click();
  await page.getByRole("button", { name: "Remove registration", exact: true }).click();
  await expect(page.getByRole("status")).toContainText("Weight files retained");
  await navigate(page, "Library");
  await expect(set.getByRole("button", { name: "Remove set", exact: true })).toBeEnabled();
  await set.getByRole("button", { name: "Remove set", exact: true }).click();
  await expect(set).toHaveCount(0);
  await expect(artifact).toBeVisible();
  await expect(artifact.getByRole("button", { name: "Remove weights", exact: true })).toBeEnabled();
  await artifact.getByRole("button", { name: "Remove weights", exact: true }).click();
  await expect(artifact).toHaveCount(0);
});

test("removed artifact sets preserve registration drafts and explain disabled submission", async ({ page, request }) => {
  const fixture = await sourceFixture(request);
  await navigate(page, "Library");
  const job = await acquire(page, fixture);
  await expect(job.locator(".badge")).toHaveText("complete", { timeout: 10000 });
  const publish = async (companions: boolean) => {
    const inventory = await (await request.get("/v1/juiced/artifacts")).json();
    const response = await request.post("/v1/juiced/artifacts/sets", { data: {
      expected_generation: inventory.generation,
      specification: { schema_version: 1, origin: { importer_id: "browser-fixture", importer_version: "1", source_revision: randomUUID() }, primary: "model",
        members: [{ name: "model", role: "model", artifact: fixture.sha256, bytes: fixture.bytes }, ...(companions ? [{ name: "projector", role: "projector", artifact: fixture.sha256, bytes: fixture.bytes }] : [])] },
    } });
    expect(response.ok(), await response.text()).toBe(true);
    return response.json();
  };
  const companion = await publish(true);
  const draft = await publish(false);
  await page.getByRole("button", { name: "Refresh library", exact: true }).click();
  const companionRow = page.getByRole("article", { name: `Artifact set ${companion.id}`, exact: true });
  await expect(companionRow).toContainText("2 members");
  await expect(companionRow.getByRole("button", { name: "Register model", exact: true })).toBeDisabled();
  const inventory = await (await request.get("/v1/juiced/artifacts")).json();
  expect(inventory.artifacts.filter((file: any) => file.id === fixture.sha256)).toHaveLength(1);
  const draftRow = page.getByRole("article", { name: `Artifact set ${draft.id}`, exact: true });
  await draftRow.getByRole("button", { name: "Register model", exact: true }).click();
  const form = page.getByRole("form", { name: "Register managed model" });
  await form.getByLabel("New model ID", { exact: true }).fill("preserved.draft");
  const removed = await request.post("/v1/juiced/artifacts/sets/remove", { data: { set: draft.id, expected_generation: inventory.generation } });
  expect(removed.ok(), await removed.text()).toBe(true);
  await page.getByRole("button", { name: "Refresh library", exact: true }).click();
  await expect(form.getByRole("alert")).toContainText("Set removed. Draft kept.");
  await expect(form.getByLabel("New model ID", { exact: true })).toHaveValue("preserved.draft");
  await page.evaluate(() => { document.documentElement.style.fontSize = "32px"; });
  const submit = form.getByRole("button", { name: "Register Disabled model", exact: true });
  await expect(submit).toBeDisabled();
  await submit.scrollIntoViewIfNeeded();
  await expect(form.getByRole("alert")).toBeInViewport();
  expect(await page.evaluate(() => document.body.scrollWidth <= innerWidth)).toBe(true);
  await page.evaluate(() => { document.documentElement.style.fontSize = ""; });
  await form.getByRole("button", { name: "Close registration", exact: true }).click();
  await companionRow.getByRole("button", { name: "Remove set", exact: true }).click();
  await expect(companionRow).toHaveCount(0);
  const artifact = page.getByRole("article", { name: `Weight file ${fixture.sha256}`, exact: true });
  await expect(artifact.getByRole("button", { name: "Remove weights", exact: true })).toBeEnabled();
  await artifact.getByRole("button", { name: "Remove weights", exact: true }).click();
  await expect(artifact).toHaveCount(0);
});

test("library pages keep off-page registration drafts and restart after membership changes", async ({ page, request }, info) => {
  const fixture = await sourceFixture(request);
  await navigate(page, "Library");
  const job = await acquire(page, fixture);
  await expect(job.locator(".badge")).toHaveText("complete", { timeout: 10000 });
  const publish = async () => {
    const inventory = await (await request.get("/v1/juiced/artifacts")).json();
    const response = await request.post("/v1/juiced/artifacts/sets", { data: {
      expected_generation: inventory.generation,
      specification: { schema_version: 1, origin: { importer_id: "browser-paging", importer_version: "1", source_revision: randomUUID() }, primary: "model", members: [{ name: "model", role: "model", artifact: fixture.sha256, bytes: fixture.bytes }] },
    } });
    expect(response.ok(), await response.text()).toBe(true);
    return (await response.json()).id as string;
  };
  const remove = async (set: string) => {
    const inventory = await (await request.get("/v1/juiced/artifacts")).json();
    const response = await request.post("/v1/juiced/artifacts/sets/remove", { data: { set, expected_generation: inventory.generation } });
    expect(response.ok(), await response.text()).toBe(true);
  };
  const created: string[] = [];
  for (let index = 0; index < 65; index++) created.push(await publish());
  const first = await (await request.get("/v1/juiced/artifacts?section=artifact_sets&limit=64")).json();
  const second = await (await request.get(`/v1/juiced/artifacts?section=artifact_sets&limit=64&artifact_sets_after=${first.pages.artifact_sets.next_after}&expected_view=${first.view}`)).json();
  const offPage: string = second.artifact_sets.at(-1).id;
  expect(first.artifact_sets.map((set: { id: string }) => set.id)).not.toContain(offPage);

  await page.getByRole("button", { name: "Refresh library", exact: true }).click();
  const pager = page.locator('[aria-label="Set pages"]');
  await expect(pager).toContainText(`Page 1 · ${first.pages.artifact_sets.total} total`);
  await expect(page.getByRole("region", { name: "Artifact sets" }).getByRole("article")).toHaveCount(64);
  await pager.getByRole("button", { name: "Next set page", exact: true }).click();
  await expect(pager).toContainText("Page 2");
  const row = page.getByRole("article", { name: `Artifact set ${offPage}`, exact: true });
  await row.getByRole("button", { name: "Register model", exact: true }).click();
  const form = page.getByRole("form", { name: "Register managed model" });
  await form.getByLabel("New model ID", { exact: true }).fill("paged.draft");
  await expect(form).toContainText(`Artifact set: ${offPage}`);
  await pager.getByRole("button", { name: "Previous set page", exact: true }).click();
  await expect(pager).toContainText("Page 1");
  await expect(row).toHaveCount(0);
  await expect(form.getByLabel("New model ID", { exact: true })).toHaveValue("paged.draft");
  await expect(form.getByRole("alert")).toHaveCount(0);
  await expect(form.getByRole("button", { name: "Register Disabled model", exact: true })).toBeEnabled();
  await page.evaluate(() => { document.documentElement.style.fontSize = "32px"; });
  await pager.scrollIntoViewIfNeeded();
  expect(await page.evaluate(() => document.body.scrollWidth <= innerWidth)).toBe(true);
  await page.screenshot({ path: info.outputPath("artifact-set-pages.png") });
  await page.evaluate(() => { document.documentElement.style.fontSize = ""; });

  await pager.getByRole("button", { name: "Next set page", exact: true }).click();
  await expect(pager).toContainText("Page 2");
  created.push(await publish());
  await page.getByRole("button", { name: "Refresh library", exact: true }).click();
  const restart = page.getByRole("button", { name: "Return to first pages", exact: true });
  await expect(restart).toBeVisible();
  await expect(form.getByLabel("New model ID", { exact: true })).toHaveValue("paged.draft");
  await restart.click();
  await expect(restart).toHaveCount(0);
  await expect(pager).toContainText(`Page 1 · ${first.pages.artifact_sets.total + 1} total`);

  await remove(offPage);
  await page.getByRole("button", { name: "Refresh library", exact: true }).click();
  await expect(form.getByRole("alert")).toContainText("Set removed. Draft kept.");
  await expect(form.getByLabel("New model ID", { exact: true })).toHaveValue("paged.draft");
  await expect(form.getByRole("button", { name: "Register Disabled model", exact: true })).toBeDisabled();
  await form.getByRole("button", { name: "Close registration", exact: true }).click();
  for (const set of created.filter((set) => set !== offPage)) await remove(set);
  await page.getByRole("button", { name: "Refresh library", exact: true }).click();
  await expect(pager).toHaveCount(0);
  const artifact = page.getByRole("article", { name: `Weight file ${fixture.sha256}`, exact: true });
  await expect(artifact.getByRole("button", { name: "Remove weights", exact: true })).toBeEnabled();
  await artifact.getByRole("button", { name: "Remove weights", exact: true }).click();
  await expect(artifact).toHaveCount(0);
});

test("model revision drafts require explicit refresh after another client changes policy and preserve rollback", async ({ page, request }, info) => {
  const fixture = await sourceFixture(request);
  const registered = `browser.revision-${info.project.name}`;
  await navigate(page, "Library");
  const job = await acquire(page, fixture);
  await expect(job.locator(".badge")).toHaveText("complete", { timeout: 10000 });
  const file = page.getByRole("article", { name: `Weight file ${fixture.sha256}`, exact: true });
  await file.getByRole("button", { name: "Create model set", exact: true }).click();
  const form = page.getByRole("form", { name: "Register managed model" });
  await form.getByLabel("New model ID", { exact: true }).fill(registered);
  await form.getByLabel("Engine executable", { exact: true }).fill(fixture.command);
  await form.getByLabel("Engine arguments as JSON").fill(JSON.stringify(fixture.args));
  await form.getByLabel("Resident reservation in bytes", { exact: true }).fill("1048576");
  await form.getByRole("button", { name: "Register Disabled model", exact: true }).click();
  await expect(form.getByRole("status")).toContainText(`Registered ${registered} as Disabled`);
  await form.getByRole("button", { name: "Close registration", exact: true }).click();
  const inventory = await (await request.get("/v1/juiced/artifacts")).json();
  const first = inventory.artifact_sets.find((set: any) => set.specification.origin.importer_id === "juiced-console");
  const published = await request.post("/v1/juiced/artifacts/sets", { data: { expected_generation: inventory.generation, specification: { ...first.specification, origin: { importer_id: "browser-revision-fixture", importer_version: "1", source_revision: randomUUID() } } } });
  expect(published.ok(), await published.text()).toBe(true);
  const second = await published.json();
  await page.getByRole("region", { name: "Registered library models" }).locator(".management-row").filter({ hasText: registered }).getByRole("button", { name: "Manage model", exact: true }).click();
  const inspector = page.getByRole("complementary", { name: "Model inspector" });
  const panel = inspector.getByRole("region", { name: "Model artifact revision" });
  await panel.getByLabel("Replacement artifact set", { exact: true }).selectOption(second.id);
  await expect(inspector.getByLabel("Policy", { exact: true })).toBeDisabled();
  await expect(inspector.getByRole("button", { name: "Load worker", exact: true })).toBeDisabled();
  const model = (await (await request.get("/v1/juiced/catalog")).json()).data.find((model: any) => model.id === registered);
  const changed = await request.post("/v1/juiced/policy", { data: { model: registered, policy: "on-demand", expected_revision: model.revision } });
  expect(changed.ok(), await changed.text()).toBe(true);
  await expect(inspector.getByLabel("Policy", { exact: true })).toHaveValue("on-demand");
  await expect(panel).toContainText("The model changed. Your selection is kept.");
  await expect(panel.getByLabel("Replacement artifact set", { exact: true })).toHaveValue(second.id);
  await expect(panel.getByRole("button", { name: "Use selected set", exact: true })).toBeDisabled();
  await panel.getByRole("button", { name: "Refresh revisions", exact: true }).click();
  await expect(panel.getByRole("button", { name: "Use selected set", exact: true })).toBeEnabled();
  await panel.getByRole("button", { name: "Use selected set", exact: true }).click();
  await expect(panel).toContainText(`Current set: ${second.id}`);
  await expect(panel).toContainText(`Previous set: ${first.id}`);
  await expect(inspector.getByLabel("Policy", { exact: true })).toHaveValue("on-demand");
  await expect(panel.getByRole("button", { name: "Roll back model revision", exact: true })).toBeEnabled();
  await panel.getByRole("button", { name: "Roll back model revision", exact: true }).click();
  await expect(panel).toContainText(`Current set: ${first.id}`);
  await expect(panel).toContainText(`Previous set: ${second.id}`);
  await page.evaluate(() => { document.documentElement.style.fontSize = "32px"; });
  await panel.getByRole("button", { name: "Roll back model revision", exact: true }).scrollIntoViewIfNeeded();
  expect(await page.evaluate(() => document.body.scrollWidth <= innerWidth)).toBe(true);
  await page.screenshot({ path: info.outputPath("artifact-revision-rollback.png") });
  await page.evaluate(() => { document.documentElement.style.fontSize = ""; });
  await inspector.getByLabel("Policy", { exact: true }).selectOption("disabled");
  await inspector.getByRole("button", { name: "Save policy", exact: true }).click();
  await expect(page.getByRole("status")).toContainText("Saved Disabled policy");
  await inspector.getByRole("button", { name: "Remove registration", exact: true }).click();
  await expect(page.getByRole("status")).toContainText("Weight files retained");
  await navigate(page, "Library");
  for (const id of [first.id, second.id]) {
    const row = page.getByRole("article", { name: `Artifact set ${id}`, exact: true });
    await expect(row.getByRole("button", { name: "Remove set", exact: true })).toBeEnabled();
    await row.getByRole("button", { name: "Remove set", exact: true }).click();
    await expect(row).toHaveCount(0);
  }
  await expect(file.getByRole("button", { name: "Remove weights", exact: true })).toBeEnabled();
  await file.getByRole("button", { name: "Remove weights", exact: true }).click();
  await expect(file).toHaveCount(0);
});

test("runtime installation keeps host paths through conflicts and retries without selecting or loading models", async ({ page, request }, info) => {
  test.skip(process.platform === "win32", "Managed runtime installation needs the qualified POSIX file owner");
  const fixture = await (await request.get(`${sourceOrigin}/fixture.json`)).json();
  const root = dirname(fixture.model_file);
  const before = await (await request.get("/v1/juiced/catalog")).json();
  const inventory = await (await request.get("/v1/juiced/runtime-packages")).json();
  const source = inventory.packages.find((item: any) => item.provenance.build_revision === "fixture-runtime-first");
  expect(source).toBeTruthy();
  const specification = JSON.parse(await readFile(join(root, "runtime-import.json"), "utf8"));
  const build = `browser-install-${randomUUID()}`;
  specification.provenance.build_revision = build;
  specification.bundle.root = join(root, "runtime-packages", source.id, "payload");
  const path = join(root, `${build}.json`);
  await writeFile(path, JSON.stringify(specification));
  const routePattern = "**/v1/juiced/runtime-packages/import";
  let releaseInitial = () => {};
  const initial = new Promise<void>((resolve) => { releaseInitial = resolve; });
  await page.route("**/v1/juiced/runtime-packages", async (route) => { await initial; await route.continue(); });
  try {
    await navigate(page, "Backends & hardware");
    const panel = page.getByRole("region", { name: "Installed runtime packages", exact: true });
    const input = panel.getByLabel("Bundle specification on service host", { exact: true });
    const submit = panel.getByRole("button", { name: "Import local runtime", exact: true });
    const refresh = panel.getByRole("button", { name: "Refresh installed runtimes", exact: true });
    await expect(submit).toBeDisabled();
    await expect(input).toBeDisabled();
    releaseInitial();
    await expect(input).toBeEnabled();
    await page.unroute("**/v1/juiced/runtime-packages");
    await input.fill("relative-import.json");
    await submit.click();
    await expect(panel.getByRole("alert")).toContainText("absolute specification-file path");
    await expect(input).toHaveValue("relative-import.json");
    await input.fill(path);
    await page.route(routePattern, (route) => route.fulfill({ status: 409, json: { error: { code: "manifest_revision_conflict", message: "Another client changed the runtime generation." } } }));
    await submit.click();
    await expect(panel.getByRole("alert")).toContainText("Another client changed");
    await expect(input).toHaveValue(path);
    await page.unroute(routePattern);
    const installed = await request.post("/v1/juiced/runtime-packages/import", { data: { specification_file: path, expected_generation: inventory.generation } });
    expect(installed.ok(), await installed.text()).toBe(true);
    const added = await installed.json();
    await expect(panel).toContainText(build);
    await expect(panel).toContainText("The installed runtimes changed. Your path is kept.");
    await expect(submit).toBeDisabled();
    await refresh.click();
    await expect(submit).toBeEnabled();
    await submit.click();
    await expect(panel.getByRole("status")).toContainText(`Verified existing runtime: ${added.package}`);
    await expect(input).toHaveValue("");
    const next = structuredClone(specification);
    next.provenance.build_revision = `${build}-new`;
    await writeFile(path, JSON.stringify(next));
    await input.fill(path);
    await refresh.click();
    await submit.click();
    await expect(panel.getByRole("status")).toContainText("Installed runtime:");
    await expect(input).toHaveValue("");
    await expect(panel).toContainText(`${build}-new`);
    const catalog = await (await request.get("/v1/juiced/catalog")).json();
    expect(catalog.data.map((model: any) => ({ id: model.id, policy: model.policy, runtime_package: model.runtime_package, loaded: model.loaded }))).toEqual(before.data.map((model: any) => ({ id: model.id, policy: model.policy, runtime_package: model.runtime_package, loaded: model.loaded })));
    const active = await (await request.get("/v1/juiced/runtime-packages")).json();
    await page.route("**/v1/juiced/runtime-packages", (route) => route.fulfill({ json: { ...active, installation_active: true } }));
    await input.fill(path);
    await refresh.click();
    await expect(panel.getByRole("status").last()).toContainText("Installation is active on the service");
    await expect(submit).toBeDisabled();
    await expect(input).toHaveValue(path);
    await page.unroute("**/v1/juiced/runtime-packages");
    await page.route("**/v1/juiced/runtime-packages", (route) => route.fulfill({ status: 503, json: { error: { code: "fixture_unavailable", message: "Package inventory is temporarily unavailable." } } }));
    await input.fill(path);
    await refresh.click();
    await expect(panel.getByRole("alert").last()).toContainText("Package inventory is temporarily unavailable");
    await expect(input).toHaveValue(path);
    await expect(submit).toBeDisabled();
    await page.unroute("**/v1/juiced/runtime-packages");
    await refresh.click();
    await panel.getByRole("button", { name: "Clear import draft", exact: true }).click();
    await page.evaluate(() => { document.documentElement.style.fontSize = "32px"; });
    await panel.scrollIntoViewIfNeeded();
    expect(await panel.evaluate((element) => element.scrollWidth <= element.clientWidth)).toBe(true);
    await page.screenshot({ path: info.outputPath("runtime-installation.png") });
  } finally {
    releaseInitial();
    await page.unroute(routePattern);
    await page.unroute("**/v1/juiced/runtime-packages");
    await rm(path, { force: true });
  }
});

test("installed runtime drafts retain revision authority and preserve explicit promotion and rollback", async ({ page, request }, info) => {
  test.skip(process.platform === "win32", "Managed runtime fixture needs the qualified POSIX file owner");
  const id = "fixture.managed-runtime";
  const inventory = await (await request.get("/v1/juiced/runtime-packages")).json();
  const first = inventory.packages.find((item: any) => item.provenance.build_revision === "fixture-runtime-first");
  const second = inventory.packages.find((item: any) => item.provenance.build_revision === "fixture-runtime-second");
  expect(first.phase).toBe("installed"); expect(second.compatibility).toBe("unqualified");
  let baseline = await (await request.get(`/v1/models/${id}/runtime`)).json();
  if (baseline.worker_in_use) expect((await request.post("/v1/unload", { data: { model: id } })).ok()).toBe(true);
  if (baseline.current !== first.id) {
    const reset = await request.post(`/v1/models/${id}/runtime`, { data: { expected_revision: baseline.revision, change: { action: "promote", package: first.id } } });
    expect(reset.ok(), await reset.text()).toBe(true); baseline = await reset.json();
  }
  if (baseline.policy !== "disabled") expect((await request.post("/v1/juiced/policy", { data: { model: id, policy: "disabled", expected_revision: baseline.revision } })).ok()).toBe(true);
  await page.reload();
  await page.getByRole("button", { name: id, exact: true }).click();
  const inspector = page.getByRole("complementary", { name: "Model inspector" });
  const panel = inspector.getByRole("region", { name: "Model runtime selection" });
  await expect(panel).toContainText(`Current runtime: ${first.id}`);
  await panel.getByLabel("Replacement installed runtime", { exact: true }).selectOption(second.id);
  await expect(inspector.getByLabel("Policy", { exact: true })).toBeDisabled();
  await expect(inspector.getByRole("button", { name: "Load worker", exact: true })).toBeDisabled();
  const original = await (await request.get(`/v1/models/${id}/runtime`)).json();
  const changed = await request.post("/v1/juiced/policy", { data: { model: id, policy: "on-demand", expected_revision: original.revision } });
  expect(changed.ok(), await changed.text()).toBe(true);
  await expect(inspector.getByLabel("Policy", { exact: true })).toHaveValue("on-demand");
  await expect(panel).toContainText("The model changed. Your runtime selection is kept.");
  await expect(panel.getByLabel("Replacement installed runtime", { exact: true })).toHaveValue(second.id);
  await expect(panel.getByRole("button", { name: "Use selected runtime", exact: true })).toBeDisabled();
  await panel.getByRole("button", { name: "Refresh runtimes", exact: true }).click();
  await expect(panel.getByRole("button", { name: "Use selected runtime", exact: true })).toBeEnabled();
  await panel.getByRole("button", { name: "Use selected runtime", exact: true }).click();
  await expect(panel).toContainText(`Current runtime: ${second.id}`);
  await expect(panel).toContainText(`Previous runtime: ${first.id}`);
  expect((await (await request.get(`/v1/models/${id}/runtime`)).json()).worker_in_use).toBe(false);
  await page.evaluate(() => { document.documentElement.style.fontSize = "32px"; });
  await panel.getByRole("button", { name: "Roll back runtime", exact: true }).scrollIntoViewIfNeeded();
  expect(await page.evaluate(() => document.body.scrollWidth <= innerWidth)).toBe(true);
  expect(await inspector.locator(".inspector-body").evaluate((body) => body.clientHeight >= Math.min(6 * parseFloat(getComputedStyle(document.documentElement).fontSize), innerHeight * 0.4))).toBe(true);
  const rollback = panel.getByRole("button", { name: "Roll back runtime", exact: true });
  expect(await rollback.evaluate((button) => { const rect = button.getBoundingClientRect(); const hit = document.elementFromPoint(rect.x + rect.width / 2, rect.y + rect.height / 2); return hit === button || button.contains(hit); })).toBe(true);
  await page.screenshot({ path: info.outputPath("installed-runtime-history.png") });
  await rollback.click();
  await page.evaluate(() => { document.documentElement.style.fontSize = ""; });
  await expect(panel).toContainText(`Current runtime: ${first.id}`);
  await expect(panel).toContainText(`Previous runtime: ${second.id}`);
  await inspector.getByRole("button", { name: "Load worker", exact: true }).click();
  await expect(page.getByRole("status")).toContainText(`Loaded ${id}`);
  await expect(panel.getByLabel("Replacement installed runtime", { exact: true })).toBeDisabled();
  await expect(panel.getByRole("button", { name: "Roll back runtime", exact: true })).toBeDisabled();
  await inspector.getByRole("button", { name: "Unload", exact: true }).click();
  await expect(page.getByRole("status")).toContainText(`Unloaded ${id}`);
  await expect(panel.getByRole("button", { name: "Roll back runtime", exact: true })).toBeEnabled();
  await panel.getByLabel("Replacement installed runtime", { exact: true }).selectOption(second.id);
  await page.route("**/v1/juiced/runtime-packages", (route) => route.fulfill({ json: { ...inventory, packages: inventory.packages.map((item: any) => item.id === second.id ? { ...item, declared_host_match: false } : item) } }));
  await panel.getByRole("button", { name: "Refresh runtimes", exact: true }).click();
  await expect(panel).toContainText("does not match this adapter and service target");
  await expect(panel.getByLabel("Replacement installed runtime", { exact: true })).toHaveValue(second.id);
  await expect(panel.getByRole("button", { name: "Use selected runtime", exact: true })).toBeDisabled();
  await page.unroute("**/v1/juiced/runtime-packages");
  await panel.getByRole("button", { name: "Refresh runtimes", exact: true }).click();
  await expect(panel.getByRole("button", { name: "Use selected runtime", exact: true })).toBeEnabled();
  await page.route("**/v1/juiced/runtime-packages", (route) => route.fulfill({ json: { schema_version: 1, packages: null } }));
  await panel.getByRole("button", { name: "Refresh runtimes", exact: true }).click();
  await expect(panel.getByRole("alert")).toContainText("runtime inventory is invalid");
  await expect(panel.getByRole("button", { name: "Roll back runtime", exact: true })).toBeDisabled();
  await expect(panel.getByLabel("Replacement installed runtime", { exact: true })).toHaveValue(second.id);
  await expect(panel.getByRole("button", { name: "Use selected runtime", exact: true })).toBeDisabled();
  await page.unroute("**/v1/juiced/runtime-packages");
  await panel.getByRole("button", { name: "Cancel runtime draft", exact: true }).click();
  await panel.getByRole("button", { name: "Refresh runtimes", exact: true }).click();
  await expect(panel.getByRole("alert")).toHaveCount(0);
  await inspector.getByLabel("Policy", { exact: true }).selectOption("disabled");
  await inspector.getByRole("button", { name: "Save policy", exact: true }).click();
  await expect(page.getByRole("status")).toContainText("Saved Disabled policy");
});
