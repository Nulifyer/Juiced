import { api, expect, largeText, modelId, navigate, noHorizontalOverflow, open, storedText, test } from "./support";

test.afterEach(async ({ request }) => {
  const service = api(request);
  const cache = await service.json<{ enabled: boolean; configuration_generation: number }>("/v1/juiced/cache");
  if (cache.enabled) await service.post("/v1/juiced/cache", { enabled: false, expected_generation: cache.configuration_generation });
});

test("snapshots save, restore by session, check storage, forget and recover a retained reservation", async ({ page, request }) => {
  const service = api(request);
  await open(page);
  await navigate(page, "Cache");
  const summary = page.getByRole("region", { name: "Prompt snapshots" });
  const toggle = summary.getByRole("switch", { name: "Snapshots" });
  await expect(toggle).toHaveAttribute("aria-checked", "false");
  await toggle.click();
  await expect(toggle).toHaveAttribute("aria-checked", "true");
  await expect(page.getByRole("status").filter({ hasText: "Snapshots turned on." })).toBeVisible();
  expect((await service.json<{ enabled: boolean }>("/v1/juiced/cache")).enabled).toBe(true);

  const session = page.getByRole("region", { name: "Snapshot session" });
  const saveButton = session.getByRole("button", { name: "Save a snapshot" });
  await expect(saveButton).toBeDisabled();
  await session.getByLabel("Snapshot model").selectOption(modelId);
  await session.getByLabel("Session ID").fill("browser-private-context");
  await expect(saveButton).toBeEnabled();
  await saveButton.click();
  const dialog = page.getByRole("dialog", { name: "Save a snapshot" });
  await expect(dialog).toContainText(`${modelId} · session browser-priv…`);
  await expect(dialog.getByRole("button", { name: "Save slot 0 snapshot" })).toBeDisabled();
  await dialog.getByLabel("Prompt").fill("Temporary browser pipeline context");
  await dialog.getByRole("button", { name: "Complete session request" }).click();
  await expect(dialog.getByText("Prompt processed.", { exact: true })).toBeVisible({ timeout: 15000 });
  await expect(dialog).toContainText("Rust fixture reply.");
  await dialog.getByRole("button", { name: "Save slot 0 snapshot" }).click();
  await expect(dialog).toHaveCount(0);
  await expect(page.getByRole("status").filter({ hasText: "Snapshot saved." })).toBeVisible();
  const table = page.getByRole("table", { name: "Snapshots" });
  const snapshot = table.getByRole("row");
  await expect(snapshot).toHaveCount(1);
  await expect(snapshot).toContainText("saved");
  await expect(summary).toContainText("1 saved");

  await session.getByLabel("Session ID").fill("wrong-context");
  await snapshot.getByRole("button", { name: "Restore snapshot" }).click();
  await expect(page.getByRole("alert")).toBeVisible();
  await expect(session.getByLabel("Session ID")).toHaveValue("wrong-context");
  await session.getByLabel("Session ID").fill("browser-private-context");
  await snapshot.getByRole("button", { name: "Restore snapshot" }).click();
  await expect(page.getByRole("status").filter({ hasText: "Snapshot restored." })).toBeVisible();
  await expect(page.getByRole("alert")).toHaveCount(0);

  await summary.getByRole("button", { name: "Check storage" }).click();
  const inspection = page.getByRole("region", { name: "Retained cache storage" });
  await expect(inspection).toContainText("Checksum OK");
  await expect(page.getByRole("status").filter({ hasText: "Storage check finished." })).toBeVisible();

  await snapshot.getByRole("button", { name: "Forget snapshot" }).click();
  await expect(page.getByRole("status").filter({ hasText: "Snapshot forgotten." })).toBeVisible();
  await expect(snapshot).toHaveCount(0);
  await expect(page.getByRole("heading", { name: "No snapshots" })).toBeVisible();

  await saveButton.click();
  await dialog.getByLabel("Prompt").fill("Temporary browser pipeline context");
  await dialog.getByRole("button", { name: "Complete session request" }).click();
  await expect(dialog.getByText("Prompt processed.", { exact: true })).toBeVisible({ timeout: 15000 });
  await dialog.getByLabel("Snapshot ceiling in bytes").fill("1");
  await dialog.getByRole("button", { name: "Save slot 0 snapshot" }).click();
  await expect(dialog.getByRole("alert")).toBeVisible();
  await dialog.getByRole("button", { name: "Close", exact: true }).click();
  await expect(snapshot).toHaveCount(1);
  const recover = snapshot.getByRole("button", { name: "Recover retained reservation" });
  await recover.click();
  await expect(page.getByRole("alert")).toBeVisible();
  await expect(snapshot).toHaveCount(1);
  await service.unload(modelId);
  await recover.click();
  await expect(page.getByRole("status").filter({ hasText: "Recovered the incomplete snapshot's space." })).toBeVisible();
  await expect(snapshot).toHaveCount(0);

  await toggle.click();
  await expect(toggle).toHaveAttribute("aria-checked", "false");
  await expect(page.getByRole("status").filter({ hasText: "Snapshots turned off. Saved files are kept." })).toBeVisible();
  expect(await storedText(page)).not.toContain("browser-private-context");
  const saved = await service.json<{ enabled: boolean; data: unknown[] }>("/v1/juiced/cache");
  expect(saved.enabled).toBe(false);
  expect(saved.data).toHaveLength(0);
});

test("snapshot pages continue with their view and restart after the cache changes", async ({ page }) => {
  const row = (index: number) => ({ id: index.toString(16).padStart(32, "0"), revision: 2, state: "saved", reserved_bytes: 64, bytes: 64, protocol: "openai.completions", session_digest: "c".repeat(64), active_readers: 0, retained_readers: [] });
  const first = Array.from({ length: 128 }, (_, index) => row(index + 1));
  const last = row(129);
  let changed = false;
  const requests: string[] = [];
  const view = () => changed ? "b".repeat(64) : "a".repeat(64);
  const generation = () => changed ? 8 : 7;
  await page.route("**/v1/juiced/cache*", async (route) => {
    if (route.request().method() !== "GET") return route.continue();
    const query = new URL(route.request().url()).searchParams;
    requests.push(route.request().url());
    const after = query.get("after");
    if (after && (query.get("expected_generation") !== String(generation()) || query.get("expected_view") !== view())) {
      return route.fulfill({ status: 409, json: { error: { code: "cache_revision_conflict", message: "The cache changed. Refresh from the first page." } } });
    }
    await route.fulfill({ json: { schema_version: 1, enabled: true, configuration_generation: 1, generation: generation(), qualification: "unqualified", data: after ? [last] : first, page: { view: view(), limit: 128, next_after: after ? null : first.at(-1)!.id } } });
  });
  await page.route("**/v1/juiced/cache/inspect*", async (route) => {
    const after = new URL(route.request().url()).searchParams.get("after");
    await route.fulfill({ json: { generation: generation(), read_only: true, recovery_authorized: false, charged_bytes: 129 * 64, snapshots: (after ? [last] : first).map((snapshot) => ({ snapshot, issues: [], checksum_valid: true })), orphan_files: ["kv-observed.bin"], issues: [], page: { view: view(), limit: 128, next_after: after ? null : first.at(-1)!.id }, orphan_page: { limit: 128, next_after: null, observed_count: 200, scan_complete: false } } });
  });
  await open(page);
  await navigate(page, "Cache");
  const table = page.getByRole("table", { name: "Snapshots" });
  const pager = page.getByLabel("Snapshot pages");
  await expect(table.getByRole("row")).toHaveCount(128);
  await page.getByRole("region", { name: "Snapshot session" }).getByLabel("Session ID").fill("private-page-session");
  await pager.getByRole("button", { name: "Next snapshot page" }).click();
  await expect(pager).toContainText("Page 2");
  await expect(table.getByRole("row", { name: `Snapshot ${last.id}` })).toBeVisible();
  await expect(table.getByRole("row")).toHaveCount(1);
  await expect(pager.getByRole("button", { name: "Next snapshot page" })).toBeDisabled();
  await pager.getByRole("button", { name: "Previous snapshot page" }).click();
  await expect(pager).toContainText("Page 1");
  await pager.getByRole("button", { name: "Next snapshot page" }).click();
  await expect(pager).toContainText("Page 2");
  changed = true;
  await expect(page.getByRole("alert").filter({ hasText: "The cache changed" })).toContainText("Showing the last observation.", { timeout: 10000 });
  await expect(page.getByText("Showing an older page.")).toBeVisible();
  await page.getByRole("button", { name: "Restart snapshot pages" }).click();
  await expect(pager).toContainText("Page 1");
  await expect(table.getByRole("row")).toHaveCount(128);
  await expect(page.getByRole("alert")).toHaveCount(0);
  await page.getByRole("button", { name: "Check storage" }).click();
  const inspection = page.getByRole("region", { name: "Retained cache storage" });
  await expect(inspection).toContainText("200 untracked files (scan incomplete)");
  await inspection.getByRole("button", { name: "Next inspection page" }).click();
  await expect(inspection).toContainText("Page 2");
  await expect(inspection.getByText(last.id, { exact: true })).toBeVisible();
  await expect(page.getByRole("region", { name: "Snapshot session" }).getByLabel("Session ID")).toHaveValue("private-page-session");
  expect(requests.every((url) => !url.includes("private-page-session"))).toBe(true);
  await largeText(page);
  await noHorizontalOverflow(page, "Cache pages");
});
