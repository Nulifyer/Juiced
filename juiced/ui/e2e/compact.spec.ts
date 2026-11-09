import { test, expect, openModel } from './fixture';
const captures = process.env.JUICED_DESIGN_CAPTURES;
test('compact console screens retain navigation and bounded layouts', async ({ page, runtime }) => {
  for (const width of [1440, 1024, 390]) {
    await page.setViewportSize({ width, height: 960 });
    await page.goto('/');
    for (const screen of ['Models', 'Loading policy', 'Library', 'Backends & hardware', 'Cache', 'Service APIs', 'Diagnostics', 'Test services']) {
      if (width < 700) await page.getByRole('button', { name: 'Toggle navigation' }).click();
      await page.getByRole('button', { name: screen, exact: true }).first().click();
      await expect(page.locator('main')).toBeVisible();
      await page.waitForTimeout(150);
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
      expect(await page.evaluate(() => document.documentElement.scrollHeight <= innerHeight + 1)).toBe(true);
      if (captures && width !== 1024) await page.screenshot({ path: `${captures}/${screen.replace(/[^a-z]/gi, '-')}-${width}.png` });
    }
  }
  expect(runtime.writes).toEqual([]);
});
test('model settings retain their controls at desktop and phone sizes', async ({ page, runtime }) => {
  for (const width of [1440, 390]) {
    await page.setViewportSize({ width, height: 960 });
    await openModel(page);
    await expect(page.getByLabel('Total context tokens', { exact: true })).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    if (captures) await page.screenshot({ path: `${captures}/settings-${width}.png` });
    await page.getByRole('button', { name: 'Back to model list' }).click();
  }
  expect(runtime.writes).toEqual([]);
});

for (const width of [1440, 390]) test(`field help opens by keyboard, closes with Escape and retains the draft at ${width}px`, async ({ page, runtime }) => {
  await page.setViewportSize({ width, height: 960 });
  await openModel(page);
  const input = page.getByLabel('Total context tokens', { exact: true });
  await page.getByText('Total context tokens', { exact: true }).click();
  await expect(input).toBeFocused();
  expect(await page.locator('label[for="model-context-tokens"]').evaluate((label: HTMLLabelElement) => label.control?.id)).toBe('model-context-tokens');
  await input.fill('16384');
  const trigger = page.getByRole('button', { name: 'About Total context tokens', exact: true });
  await trigger.focus();
  await page.keyboard.press('Enter');
  const popup = page.getByRole('dialog', { name: 'Total context tokens', exact: true });
  await expect(popup).toContainText('reasoning and output');
  const bounds = await popup.boundingBox();
  expect(bounds!.x).toBeGreaterThanOrEqual(0);
  expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(width);
  if (captures) await page.screenshot({ path: `${captures}/help-${width}.png` });
  await page.keyboard.press('Escape');
  await expect(trigger).toBeFocused();
  await expect(input).toHaveValue('16384');
  await page.getByRole('button', { name: 'Advanced', exact: true }).click();
  await expect(page.getByLabel('Additional llama.cpp arguments', { exact: true })).toBeVisible();
  await page.getByRole('button', { name: 'Settings', exact: true }).click();
  await expect(input).toHaveValue('16384');
  expect(runtime.writes).toEqual([]);
});

test('service protocols expose the selected connection and copy its URL', async ({ page, context, runtime }) => {
  await context.grantPermissions(['clipboard-read', 'clipboard-write']);
  await page.goto('/');
  await page.getByRole('button', { name: 'Service APIs', exact: true }).click();
  await page.getByRole('button', { name: 'Ollama', exact: true }).click();
  await expect(page.locator('.endpoint-address code')).toHaveText(new URL(page.url()).origin);
  await page.getByRole('button', { name: 'Copy base URL', exact: true }).click();
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe(new URL(page.url()).origin);
  await page.getByRole('button', { name: 'Wyoming', exact: true }).click();
  await expect(page.getByRole('region', { name: 'Client connection' })).toContainText('10300');
  await expect(page.getByRole('button', { name: 'Copy base URL', exact: true })).toHaveCount(0);
  await page.getByRole('button', { name: 'About Wyoming compatibility', exact: true }).click();
  await expect(page.getByRole('dialog', { name: 'Wyoming compatibility' })).toContainText("Home Assistant");
  expect(runtime.writes).toEqual([]);
});

test('cache policy drafts survive tab changes and save only on request', async ({ page, runtime }) => {
  await page.goto('/');
  await page.getByRole('button', { name: 'Cache', exact: true }).click();
  await page.getByRole('button', { name: 'Storage policy', exact: true }).click();
  expect(await page.locator('label[for="cache-expiry-days"]').evaluate((label: HTMLLabelElement) => label.control?.id)).toBe('cache-expiry-days');
  await page.getByLabel('Expiry (days)').fill('14');
  await page.getByRole('button', { name: /^Model overrides/ }).click();
  await expect(page.getByRole('button', { name: 'Save changes', exact: true })).toBeEnabled();
  await page.getByRole('button', { name: /^Storage policy/ }).click();
  await expect(page.getByLabel('Expiry (days)')).toHaveValue('14');
  await page.getByRole('button', { name: 'About Cache expiry', exact: true }).click();
  await expect(page.getByRole('dialog', { name: 'Cache expiry' })).toBeVisible();
  await page.keyboard.press('Escape');
  expect(runtime.writes).toEqual([]);
  await page.getByRole('button', { name: 'Save changes', exact: true }).click();
  await expect.poll(() => runtime.cache.settings.retention_days).toBe(14);
  await expect(page.getByRole('button', { name: 'Save changes', exact: true })).toBeDisabled();
});


test('selecting the current destination closes mobile navigation', async ({ page, runtime }) => {
  await page.setViewportSize({ width: 390, height: 960 });
  await page.goto('/');
  await page.getByRole('button', { name: 'Toggle navigation' }).click();
  await page.getByRole('button', { name: 'Models', exact: true }).click();
  await expect(page.locator('.app-sidebar.mobile-open')).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Toggle navigation' })).toHaveAttribute('aria-expanded', 'false');
  expect(runtime.writes).toEqual([]);
});

for (const width of [1440, 1024, 390]) test(`runtime details float above the footer without changing its layout at ${width}px`, async ({ page, runtime }) => {
  await page.setViewportSize({ width, height: 960 });
  await page.goto('/');
  const footer = page.getByLabel('Runtime hardware and inference metrics');
  await expect(footer).toContainText('35.0 tok/s');
  const before = await footer.boundingBox();
  const trigger = page.getByRole('button', { name: 'Runtime details', exact: true });
  await trigger.focus();
  await page.keyboard.press('Enter');
  const popup = page.getByRole('dialog', { name: 'Last request', exact: true });
  await expect(popup).toBeVisible();
  await expect(popup.locator('dd')).toHaveText(['12', '32', '0.3 s', '2']);
  await expect(footer).not.toContainText('Input tokens');
  const bounds = await popup.boundingBox();
  const after = await footer.boundingBox();
  expect(after!.height).toBeCloseTo(before!.height, 1);
  expect(bounds!.x).toBeGreaterThanOrEqual(0);
  expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(width);
  expect(bounds!.y + bounds!.height).toBeLessThanOrEqual(after!.y);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  if (captures) await page.screenshot({ path: `${captures}/runtime-details-${width}.png` });
  await page.keyboard.press('Escape');
  await expect(popup).not.toBeVisible();
  await expect(trigger).toBeFocused();
  await trigger.click();
  await page.getByRole('button', { name: 'Close runtime details' }).click();
  await expect(popup).not.toBeVisible();
  await trigger.click();
  await page.locator('main').click({ position: { x: 5, y: 5 } });
  await expect(popup).not.toBeVisible();
  expect(runtime.writes).toEqual([]);
});

test('runtime details keep unavailable telemetry explicit', async ({ page, runtime }) => {
  await page.route('**/v1/stats', route => route.abort());
  await page.goto('/');
  await page.getByRole('button', { name: 'Runtime details', exact: true }).click();
  const popup = page.getByRole('dialog', { name: 'Last request', exact: true });
  await expect(popup).toContainText('Telemetry unavailable. Reconnecting');
  await expect(popup.locator('dd')).toHaveText(['—', '—', '—', '—']);
  expect(runtime.writes).toEqual([]);
});

for (const width of [1440, 390]) test(`cache model overrides list only explicit settings at ${width}px`, async ({ page, runtime }) => {
  const first = runtime.models[0].id, second = runtime.models[1].id;
  runtime.cache.settings.model_modes = { [first]: 'memory', 'Unavailable-model': 'persistent' };
  await page.setViewportSize({ width, height: 960 });
  await page.goto('/');
  if (width < 700) await page.getByRole('button', { name: 'Toggle navigation' }).click();
  await page.getByRole('button', { name: 'Cache', exact: true }).click();
  await page.getByRole('button', { name: /^Model overrides/ }).click();
  const rows = page.locator('.cache-override-row');
  await expect(rows).toHaveCount(2);
  await expect(page.getByLabel(`Cache mode for ${first}`, { exact: true })).toHaveValue('memory');
  await expect(page.getByLabel('Cache mode for Unavailable-model', { exact: true })).toHaveValue('persistent');
  await expect(page.getByLabel(`Cache mode for ${second}`, { exact: true })).toHaveCount(0);
  await page.getByLabel('Model', { exact: true }).selectOption(second);
  await page.locator('.cache-override-add').getByLabel('Reuse mode', { exact: true }).selectOption('persistent');
  await page.getByRole('button', { name: 'Add override', exact: true }).click();
  await expect(rows).toHaveCount(3);
  await expect(page.getByLabel('Model', { exact: true }).locator(`option[value="${second}"]`)).toHaveCount(0);
  expect(runtime.writes).toEqual([]);
  await page.getByRole('button', { name: /^Storage policy/ }).click();
  await page.getByRole('button', { name: /^Model overrides/ }).click();
  await expect(page.getByLabel(`Cache mode for ${second}`, { exact: true })).toHaveValue('persistent');
  await page.getByRole('button', { name: 'Save changes', exact: true }).click();
  await expect.poll(() => runtime.cache.settings.model_modes[second]).toBe('persistent');
  if (captures) await page.screenshot({ path: `${captures}/cache-overrides-${width}.png` });
  await page.getByRole('button', { name: `Use default for ${first}`, exact: true }).click();
  await expect(page.getByLabel('Model', { exact: true })).toBeFocused();
  await expect(rows).toHaveCount(2);
  await page.getByRole('button', { name: 'Cancel', exact: true }).click();
  await expect(rows).toHaveCount(3);
  for (const id of [first, second, 'Unavailable-model'])
    await page.getByRole('button', { name: `Use default for ${id}`, exact: true }).click();
  await expect(rows).toHaveCount(0);
  await expect(page.getByText('No model overrides. All models use the default reuse mode.', { exact: true })).toBeVisible();
  await page.getByRole('button', { name: 'Save changes', exact: true }).click();
  await expect.poll(() => runtime.cache.settings.model_modes).toEqual({});
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  if (captures) await page.screenshot({ path: `${captures}/cache-overrides-empty-${width}.png` });
});

test('failed cache override save keeps the draft for retry', async ({ page, runtime }) => {
  let fail = true;
  await page.route('**/v1/juiced/cache', route => route.request().method() === 'POST' && fail
    ? route.fulfill({ status: 503, json: { error: 'Fixture cache save failed' } }) : route.fallback());
  await page.goto('/');
  await page.getByRole('button', { name: 'Cache', exact: true }).click();
  await page.getByRole('button', { name: /^Model overrides/ }).click();
  await expect(page.locator('.cache-override-row')).toHaveCount(0);
  await page.getByLabel('Model', { exact: true }).selectOption(runtime.models[0].id);
  await page.getByRole('button', { name: 'Add override', exact: true }).click();
  await page.getByRole('button', { name: 'Save changes', exact: true }).click();
  await expect(page.getByText('Fixture cache save failed', { exact: true })).toBeVisible();
  await expect(page.getByLabel(`Cache mode for ${runtime.models[0].id}`, { exact: true })).toHaveValue('memory');
  expect(runtime.cache.settings.model_modes).toEqual({});
  fail = false;
  await page.getByRole('button', { name: 'Save changes', exact: true }).click();
  await expect.poll(() => runtime.cache.settings.model_modes).toEqual({ [runtime.models[0].id]: 'memory' });
});
