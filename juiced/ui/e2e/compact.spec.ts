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
  await expect(page.locator('.endpoint-address code')).toHaveText('http://127.0.0.1:5187');
  await page.getByRole('button', { name: 'Copy base URL', exact: true }).click();
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe('http://127.0.0.1:5187');
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
