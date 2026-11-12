import { expect, navigate, noHorizontalOverflow, open, tab, test, toasts } from "./support";

const digest = (seed: string) => seed.repeat(64).slice(0, 64);
const view = (mode: string, models: Record<string, string> = {}, sessions = 2) => ({
  schema_version: 1, configuration_generation: 7, contains_content: false, explicit_snapshots_enabled: false,
  settings: { mode, budget_bytes: 8 * 1024 ** 3, model_budget_bytes: 4 * 1024 ** 3, max_sessions: 1024, save_delay_ms: 1000, ...(Object.keys(models).length ? { models } : {}) },
  models: [
    { model: "fixture.chat-small-q4", mode: models["fixture.chat-small-q4"] ?? mode, effective: mode, reason: null, sessions, used_bytes: 12 * 1024 ** 2, budget_bytes: 4 * 1024 ** 3, slots: [{ slot: 0, state: "holding", tokens: 3187, units: 1 }, { slot: 1, state: "empty", tokens: null, units: null }] },
    { model: "fixture.speech-tiny", mode, effective: "off", reason: "This engine does not save prompt state.", sessions: 0, used_bytes: 0, budget_bytes: 4 * 1024 ** 3, slots: [] },
  ],
  sessions: Array.from({ length: sessions }, (_, index) => ({ session: digest(String.fromCharCode(97 + index)), kind: "prefix", model: "fixture.chat-small-q4", principal: "operator", protocol: "chat", bytes: 6 * 1024 ** 2, last_used_unix_ms: Date.now() - (index + 1) * 90_000, execution_current: index === 0, state: "saved", model_tag: "abc" })),
  totals: { budget_bytes: 8 * 1024 ** 3, bytes: sessions * 6 * 1024 ** 2, listed: sessions, sessions },
});
const turns = { capacity: 128, contains_content: false, data: [
  { id: 9, model: "fixture.chat-small-q4", operation: "chat/completions", outcome: "completed", elapsed_ms: 190, prompt_processed_tokens: 1, cached_tokens: 3202, session_cache: { path: "restored", restore_ms: 93, slot: 0, key: "prefix", saved_previous: true } },
  { id: 8, model: "fixture.chat-small-q4", operation: "chat/completions", outcome: "completed", elapsed_ms: 9900, prompt_processed_tokens: 3177, cached_tokens: 0, session_cache: { path: "cold", restore_ms: null, slot: 0, key: "prefix", saved_previous: false } },
  { id: 7, model: "fixture.chat-small-q4", operation: "chat/completions", outcome: "completed", elapsed_ms: 120 },
] };

test("Sessions shows continuation per turn and changes modes and forgets sessions", async ({ page }) => {
  const posts: unknown[] = [];
  let current = view("persistent");
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
  await expect(page.getByRole("tablist", { name: "Cache views" }).getByRole("tab", { name: "Sessions" })).toHaveAttribute("aria-selected", "true");
  const summary = page.getByRole("region", { name: "Session continuation" });
  await expect(summary.getByRole("radio", { name: "Persistent" })).toHaveAttribute("aria-checked", "true");
  await expect(summary).toContainText("12.0 MiB of 8.0 GiB · 2 sessions");
  const recent = page.getByRole("region", { name: "Recent turns" });
  await expect(recent.getByRole("listitem")).toHaveCount(2);
  await expect(recent.getByRole("listitem").first()).toContainText("Restored");
  await expect(recent.getByRole("listitem").first()).toContainText("1 new · 3,202 reused · restore 93 ms · 0.19 s");
  await expect(recent.getByRole("listitem").nth(1)).toContainText("Cold");
  const models = page.getByRole("table", { name: "Session models" });
  await expect(models.getByRole("row", { name: "fixture.chat-small-q4" })).toContainText("0: holding");
  await expect(models.getByRole("row", { name: "fixture.speech-tiny" })).toContainText("This engine does not save prompt state.");

  await summary.getByRole("radio", { name: "Memory" }).click();
  await expect(toasts(page).getByRole("status")).toContainText("Session continuation is Memory.");
  expect(posts.at(-1)).toMatchObject({ action: "configure", expected_generation: 7, settings: { mode: "memory" } });
  await expect(summary.getByRole("radio", { name: "Memory" })).toHaveAttribute("aria-checked", "true");

  await models.getByLabel("Session mode for fixture.chat-small-q4").selectOption("off");
  await expect(toasts(page).getByRole("status")).toContainText("fixture.chat-small-q4 uses Off sessions.");
  expect(posts.at(-1)).toMatchObject({ action: "configure", settings: { mode: "memory", models: { "fixture.chat-small-q4": "off" } } });
  await models.getByLabel("Session mode for fixture.chat-small-q4").selectOption("inherit");
  expect((posts.at(-1) as { settings: { models?: object } }).settings.models ?? {}).toEqual({});

  const saved = page.getByRole("table", { name: "Saved session list" });
  await expect(saved.getByRole("row", { name: /^Session [a-f0-9]{8}$/ })).toHaveCount(2);
  await expect(saved.getByRole("row", { name: "Session bbbbbbbb" })).toContainText("Stale");
  await saved.getByRole("row", { name: "Session aaaaaaaa" }).getByRole("button", { name: "Forget" }).click();
  expect(posts.at(-1)).toEqual({ action: "forget", session: digest("a") });
  await expect(saved.getByRole("row", { name: /^Session [a-f0-9]{8}$/ })).toHaveCount(1);
  await page.getByRole("region", { name: "Saved sessions" }).getByRole("button", { name: "Forget all" }).click();
  expect(posts.at(-1)).toEqual({ action: "forget" });
  await expect(page.getByRole("region", { name: "Saved sessions" })).toContainText("No saved sessions");
  await noHorizontalOverflow(page, "Cache sessions");
});

test("Sessions on the real fixture lists every model and keeps manual snapshots on their own tab", async ({ page }) => {
  await open(page);
  await navigate(page, "Cache");
  await expect(page.getByRole("table", { name: "Session models" }).getByRole("row", { name: "fixture.chat-small-q4" })).toBeVisible();
  await tab(page, "Cache views", "Snapshots").click();
  await expect(page.getByRole("region", { name: "Prompt snapshots" })).toBeVisible();
});
