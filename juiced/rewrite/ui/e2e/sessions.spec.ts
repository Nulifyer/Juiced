import { expect, navigate, noHorizontalOverflow, open, test, toasts } from "./support";

const digest = (seed: string) => seed.repeat(64).slice(0, 64);
const view = (mode: string, models: Record<string, string> = {}, sessions = 2) => ({
  schema_version: 1, configuration_generation: 7, contains_content: false, explicit_snapshots_enabled: false,
  settings: { mode, budget_bytes: 8 * 1024 ** 3, model_budget_bytes: 4 * 1024 ** 3, max_sessions: 1024, save_delay_ms: 1000, ...(Object.keys(models).length ? { models } : {}) },
  models: [
    { model: "fixture.chat-small-q4", mode: models["fixture.chat-small-q4"] ?? mode, effective: mode, reason: null, sessions, used_bytes: 12 * 1024 ** 2, budget_bytes: 4 * 1024 ** 3, slots: [{ slot: 0, state: "holding", tokens: 3187, units: 1 }, { slot: 1, state: "empty", tokens: null, units: null }] },
    { model: "fixture.speech-tiny", mode, effective: "off", reason: "engine_unsupported", sessions: 0, used_bytes: 0, budget_bytes: 4 * 1024 ** 3, slots: [] },
  ],
  sessions: Array.from({ length: sessions }, (_, index) => ({ session: digest(String.fromCharCode(97 + index)), kind: index === 0 ? "header" : "prefix", model: "fixture.chat-small-q4", principal: "operator", protocol: "chat", bytes: 6 * 1024 ** 2, last_used_unix_ms: Date.now() - (index + 1) * 90_000, execution_current: index === 0 ? true : index === 1 ? false : null, state: "saved", model_tag: "abc" })),
  totals: { budget_bytes: 8 * 1024 ** 3, bytes: sessions * 6 * 1024 ** 2, listed: sessions, sessions },
});
const turns = { capacity: 128, contains_content: false, data: [
  { id: 9, model: "fixture.chat-small-q4", operation: "chat/completions", outcome: "completed", elapsed_ms: 190, prompt_processed_tokens: 1, cached_tokens: 3202, session_cache: { path: "restored", restore_ms: 93, slot: 0, key: "prefix", saved_previous: true } },
  { id: 8, model: "fixture.chat-small-q4", operation: "chat/completions", outcome: "completed", elapsed_ms: 9900, prompt_processed_tokens: 3177, cached_tokens: 0, session_cache: { path: "cold", restore_ms: null, slot: 0, key: "prefix", saved_previous: false } },
  { id: 7, model: "fixture.chat-small-q4", operation: "chat/completions", outcome: "completed", elapsed_ms: 900, prompt_processed_tokens: 4, cached_tokens: 9, session_cache: { path: "slot-hit", restore_ms: null, slot: 1, key: "prefix", saved_previous: false } },
  { id: 6, model: "fixture.chat-small-q4", operation: "chat/completions", outcome: "completed", elapsed_ms: 120 },
] };

test("Cache shows how each turn used the cache, changes modes and forgets conversations", async ({ page }) => {
  const posts: unknown[] = [];
  let current = view("persistent", {}, 3);
  await page.route("**/v1/juiced/cache/sessions", async (route) => {
    if (route.request().method() === "POST") {
      const body = route.request().postDataJSON();
      posts.push(body);
      if (body.action === "configure") current = view(body.settings.mode, body.settings.models ?? {}, current.sessions.length);
      if (body.action === "forget") current = view(current.settings.mode, current.settings.models ?? {}, body.session ? current.sessions.length - 1 : 0);
      return route.fulfill({ json: current });
    }
    return route.fulfill({ json: current });
  });
  await page.route("**/v1/juiced/requests", (route) => route.fulfill({ json: turns }));
  await open(page);
  await navigate(page, "Cache");
  await expect(page.getByRole("tablist", { name: "Cache views" })).toHaveCount(0);
  const summary = page.getByRole("region", { name: "Conversation cache" });
  await expect(summary.getByRole("radio", { name: "Memory and disk" })).toHaveAttribute("aria-checked", "true");
  await expect(summary).toContainText("18 MiB of 8 GiB used · 3 conversations");
  const recent = page.getByRole("region", { name: "Recent turns" });
  await expect(recent.getByRole("listitem")).toHaveCount(3);
  await expect(recent.getByRole("listitem").first()).toContainText("Restored from disk");
  await expect(recent.getByRole("listitem").first()).toContainText("3,202 cached + 1 new tokens · restored in 93 ms · 0.19 s");
  await expect(recent.getByRole("listitem").nth(1)).toContainText("Processed in full");
  await expect(recent.getByRole("listitem").nth(1)).toContainText("3,177 new tokens · 9.90 s");
  await expect(recent.getByRole("listitem").nth(2)).toContainText("Still loaded");
  await expect(recent.getByRole("listitem").nth(2)).toContainText("9 cached + 4 new tokens · 0.90 s");
  const overrides = page.getByRole("region", { name: "Model overrides" });
  await expect(overrides.getByRole("table", { name: "Overrides" })).toHaveCount(0);
  await expect(overrides).toContainText("None. Every model uses Memory and disk.");
  await expect(overrides.getByLabel("Model to override").getByRole("option")).toHaveText(["Choose a model…", "fixture.chat-small-q4"]);

  await summary.getByRole("radio", { name: "Memory only" }).click();
  await expect(toasts(page).getByRole("status")).toContainText("Conversation cache: Memory only.");
  expect(posts.at(-1)).toMatchObject({ action: "configure", expected_generation: 7, settings: { mode: "memory" } });
  await expect(summary.getByRole("radio", { name: "Memory only" })).toHaveAttribute("aria-checked", "true");

  await overrides.getByLabel("Model to override").selectOption("fixture.chat-small-q4");
  await overrides.getByLabel("Override mode").selectOption("off");
  await overrides.getByRole("button", { name: "Add override" }).click();
  await expect(toasts(page).getByRole("status")).toContainText("fixture.chat-small-q4 uses Off.");
  expect(posts.at(-1)).toMatchObject({ action: "configure", settings: { mode: "memory", models: { "fixture.chat-small-q4": "off" } } });
  const row = overrides.getByRole("table", { name: "Overrides" }).getByRole("row", { name: "fixture.chat-small-q4" });
  await expect(row.getByLabel("Cache mode for fixture.chat-small-q4")).toHaveValue("off");
  await expect(overrides.getByLabel("Model to override")).toHaveCount(0);
  await row.getByLabel("Cache mode for fixture.chat-small-q4").selectOption("persistent");
  expect(posts.at(-1)).toMatchObject({ action: "configure", settings: { models: { "fixture.chat-small-q4": "persistent" } } });
  await row.getByRole("button", { name: "Remove" }).click();
  await expect(toasts(page).getByRole("status")).toContainText("fixture.chat-small-q4 uses the default again.");
  expect((posts.at(-1) as { settings: { models?: object } }).settings.models ?? {}).toEqual({});
  await expect(overrides.getByRole("table", { name: "Overrides" })).toHaveCount(0);

  const saved = page.getByRole("table", { name: "Saved conversation list" });
  const conversations = saved.getByRole("row", { name: /^Conversation [a-f0-9]{8}$/ });
  await expect(conversations).toHaveCount(3);
  await expect(saved.getByRole("row", { name: "Conversation aaaaaaaa" })).toContainText("Chat · client session ID");
  await expect(saved.getByRole("row", { name: "Conversation aaaaaaaa" })).not.toContainText("Engine changed");
  await expect(saved.getByRole("row", { name: "Conversation bbbbbbbb" })).toContainText("Engine changed");
  await expect(saved.getByRole("row", { name: "Conversation cccccccc" })).not.toContainText("Engine changed");
  await saved.getByRole("row", { name: "Conversation aaaaaaaa" }).getByRole("button", { name: "Forget" }).click();
  expect(posts.at(-1)).toEqual({ action: "forget", session: digest("a") });
  await expect(conversations).toHaveCount(2);
  await page.getByRole("region", { name: "Saved conversations" }).getByRole("button", { name: "Forget all" }).click();
  expect(posts.at(-1)).toEqual({ action: "forget" });
  await expect(page.getByRole("region", { name: "Saved conversations" })).toContainText("No saved conversations");
  await noHorizontalOverflow(page, "Cache sessions");
});

test("Cache on the real fixture shows the default once and keeps manual snapshots out of the conversation list", async ({ page }) => {
  await open(page);
  await navigate(page, "Cache");
  await expect(page.getByRole("radiogroup", { name: "Default cache mode" })).toHaveCount(1);
  const overrides = page.getByRole("region", { name: "Model overrides" });
  await expect(overrides.getByRole("table", { name: "Overrides" })).toHaveCount(0);
  await expect(overrides.getByLabel("Model to override").getByRole("option")).toHaveText(["Choose a model…", "fixture.managed-runtime"]);
  const manual = page.getByRole("region", { name: "Manual snapshots" });
  await expect(manual.getByRole("switch", { name: "Allow manual save and restore" })).toHaveAttribute("aria-checked", "false");
  await expect(manual).toContainText("0 saved");
  await expect(manual.getByRole("group", { name: "Snapshot session" })).toHaveCount(0);
});
