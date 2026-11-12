import { api, expect, hotModel, largeText, modelId, navigate, noHorizontalOverflow, open, openModel, serviceMenu, storedText, test, toasts } from "./support";

const destinations = ["Dashboard", "Models", "Loading policy", "Library", "Backends & hardware", "Cache", "Test services", "Diagnostics", "Connect"];

test("every destination opens from the navigation without page errors", async ({ page }) => {
  const failures: string[] = [];
  page.on("pageerror", (error) => failures.push(error.message));
  await open(page);
  for (const name of destinations) {
    await navigate(page, name);
    await expect(page.getByRole("navigation", { name: "Destinations", includeHidden: true }).getByRole("button", { name: name === "Models" ? /^Models(?: \d+)?$/ : name, exact: name !== "Models", includeHidden: true })).toHaveAttribute("aria-current", "page");
    if (await page.getByRole("button", { name: "Toggle navigation" }).isVisible()) await expect(page.getByRole("navigation", { name: "Destinations" })).toBeHidden();
    await page.waitForTimeout(300);
    const main = page.getByRole("main");
    expect(await main.locator(".button.primary:visible").count(), `${name} has at most one primary action`).toBeLessThanOrEqual(1);
    await expect(main.locator("details"), `${name} has no folding sections`).toHaveCount(0);
    await expect(main.getByRole("button", { name: /^(Refresh|Reload)\b/ }), `${name} refreshes live data without a manual button`).toHaveCount(0);
  }
  await expect(page.getByRole("navigation", { name: "Destinations", includeHidden: true }).getByRole("button", { name: /^Models \d+$/, includeHidden: true })).toBeAttached();
  await expect(page.locator(".sidebar-version")).toHaveText("Juiced 0.1.0");
  expect(await page.evaluate(() => [...document.images].every((image) => image.complete && image.naturalWidth > 0))).toBe(true);
  expect(failures).toEqual([]);
});

test("phone and tablet navigation closes with Escape and the scrim", async ({ page }) => {
  await open(page);
  const menu = page.getByRole("button", { name: "Toggle navigation" });
  test.skip(!(await menu.isVisible()), "The desktop layout keeps navigation visible.");
  const nav = page.getByRole("navigation", { name: "Destinations" });
  await expect(nav).toBeHidden();
  await menu.click();
  await expect(nav).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(nav).toBeHidden();
  await expect(menu).toHaveAttribute("aria-expanded", "false");
  await menu.click();
  await page.getByRole("button", { name: "Close navigation" }).click({ position: { x: 300, y: 300 } });
  await expect(nav).toBeHidden();
});

test("pausing new requests shows the maintenance bar on every page until resumed", async ({ page, request }) => {
  const service = api(request);
  const before = await service.catalog();
  await open(page);
  await serviceMenu(page, "Pause new requests");
  const bar = page.getByRole("region", { name: "Maintenance admission" });
  await expect(bar).toContainText("New requests paused.");
  await expect(toasts(page).getByRole("status")).toContainText("Paused new requests.");
  await expect(page.getByRole("button", { name: "Service status" })).toContainText("Paused");
  expect((await service.json<{ paused: boolean }>("/v1/juiced/admission")).paused).toBe(true);
  expect((await service.catalog()).map(({ id, policy }) => ({ id, policy }))).toEqual(before.map(({ id, policy }) => ({ id, policy })));
  for (const name of destinations) {
    await navigate(page, name);
    await expect(bar).toBeVisible();
  }
  await serviceMenu(page, "Resume requests");
  await expect(bar).toHaveCount(0);
  await expect(toasts(page).getByRole("status")).toContainText("Resumed new requests.");
  await expect(page.getByRole("button", { name: "Service status" })).toContainText("Online");
  await serviceMenu(page, "Pause new requests");
  await expect(bar).toBeVisible();
  await bar.getByRole("button", { name: "Resume requests" }).click();
  await expect(bar).toHaveCount(0);
  expect((await service.json<{ paused: boolean }>("/v1/juiced/admission")).paused).toBe(false);
});

test("connection settings keep the API key in memory and never in local storage", async ({ page }) => {
  await open(page);
  await serviceMenu(page, "Connection settings");
  const dialog = page.getByRole("dialog", { name: "Connection settings" });
  await expect(dialog).toContainText("Connected service: 0.1.0");
  await dialog.getByLabel("Service origin").fill("not a url");
  await dialog.getByRole("button", { name: "Connect" }).click();
  await expect(dialog.getByRole("alert")).toBeVisible();
  await dialog.getByLabel("Service origin").fill("http://127.0.0.1:13307");
  await dialog.getByLabel("API key").fill("browser-memory-only-key");
  const authorized = page.waitForRequest((outgoing) => outgoing.url().includes("/v1/juiced/catalog") && outgoing.headers().authorization === "Bearer browser-memory-only-key");
  await dialog.getByRole("button", { name: "Connect" }).click();
  await expect(dialog).toHaveCount(0);
  await authorized;
  await expect(page.getByRole("region", { name: "Running models" })).toContainText(hotModel);
  const stored = await storedText(page);
  expect(stored).not.toContain("browser-memory-only-key");
  expect(stored).toContain("juiced.rewrite.base=http://127.0.0.1:13307");
  await page.reload();
  await serviceMenu(page, "Connection settings");
  await expect(dialog.getByLabel("API key")).toHaveValue("");
  await expect(dialog.getByLabel("Service origin")).toHaveValue("http://127.0.0.1:13307");
  await dialog.getByLabel("Service origin").fill("");
  await dialog.getByRole("button", { name: "Connect" }).click();
  await expect(dialog).toHaveCount(0);
  await page.getByRole("button", { name: "Service status" }).click();
  await page.getByRole("menuitem", { name: "Connection settings" }).click();
  await page.keyboard.press("Escape");
  await expect(dialog).toHaveCount(0);
});

test("success toasts dismiss themselves and the footer shows live usage", async ({ page }) => {
  await open(page);
  const footer = page.getByRole("contentinfo", { name: "Live usage" });
  await expect(footer).toContainText(/CPU\s*\d+(\.\d)?%/);
  await expect(footer).toContainText(/RAM\s*\d+(\.\d)? GiB/);
  await serviceMenu(page, "Pause new requests");
  const toast = toasts(page).getByRole("status");
  await expect(toast).toContainText("Paused new requests.");
  await expect(toast).toHaveCount(0, { timeout: 8000 });
  await serviceMenu(page, "Resume requests");
  await expect(toast).toContainText("Resumed new requests.");
  await toasts(page).getByRole("button", { name: "Dismiss notification" }).click();
  await expect(toast).toHaveCount(0);
});

test("every destination fits at 200 percent text without horizontal scrolling", async ({ page }) => {
  await open(page);
  await largeText(page);
  for (const name of destinations) {
    await navigate(page, name);
    await page.waitForTimeout(150);
    await noHorizontalOverflow(page, name);
  }
  await openModel(page, modelId);
  await noHorizontalOverflow(page, "Model inspector");
});

test("a failed refresh keeps the last observation and disables mutations", async ({ page }) => {
  await open(page);
  const inspector = await openModel(page, modelId);
  await expect(inspector.getByRole("button", { name: "Load worker" })).toBeEnabled();
  await page.route("**/v1/juiced/catalog", (route) => route.fulfill({ status: 503, json: { error: { code: "offline", message: "Fixture service unavailable. Retry the connection." } } }));
  const banner = page.getByRole("alert").filter({ hasText: "Fixture service unavailable" });
  await expect(banner).toContainText("Showing the last observation.", { timeout: 10000 });
  await expect(page.getByRole("button", { name: "Service status" })).toContainText("Offline");
  await expect(inspector.getByRole("heading", { name: modelId })).toBeVisible();
  await expect(inspector.getByRole("button", { name: "Load worker" })).toBeDisabled();
  for (const radio of await inspector.getByRole("radio").all()) await expect(radio).toBeDisabled();
  const back = page.getByRole("button", { name: "Back to models" });
  if (await back.isVisible()) await back.click();
  await expect(page.getByLabel(`Loading policy for ${modelId}`)).toBeDisabled();
  await page.unroute("**/v1/juiced/catalog");
  await expect(banner).toHaveCount(0, { timeout: 10000 });
  await expect(page.getByLabel(`Loading policy for ${modelId}`)).toBeEnabled();
});
