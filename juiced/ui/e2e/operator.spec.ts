import { test, expect, modelId, otherId, openModel } from './fixture';

test('effective settings distinguish saved, running and local edits without mutations', async ({ page, runtime }) => {
  await openModel(page);
  await page.getByLabel('Total context tokens', { exact: true }).fill('16384');
  await page.getByRole('button', { name: 'Effective', exact: true }).click();
  const row = page.getByRole('row').filter({ has: page.getByRole('rowheader', { name: /ctx size/ }) });
  await expect(row).toContainText('8192');
  await expect(row).toContainText('4096');
  await expect(row).toContainText('16384');
  await expect(page.getByText('2,048 tokens per chat', { exact: true })).toBeVisible();
  await expect(page.locator('.running-command pre')).toContainText('/runtime/llama-server');
  expect(runtime.writes).toEqual([]);
});

test('draft guard supports keep editing and discard using keyboard', async ({ page, runtime }) => {
  await openModel(page);
  await page.getByLabel('Total context tokens', { exact: true }).fill('16384');
  await page.getByRole('button', { name: 'Diagnostics', exact: true }).click();
  await expect(page.getByRole('dialog')).toBeVisible();
  await page.getByRole('button', { name: 'Keep editing' }).click();
  await expect(page.getByLabel('Total context tokens', { exact: true })).toHaveValue('16384');
  await page.getByLabel('Back to model list').click();
  await page.getByRole('button', { name: 'Discard changes' }).focus();
  await page.keyboard.press('Enter');
  await expect(page.getByRole('region', { name: 'Model details', exact: true })).toHaveCount(0);
  expect(runtime.writes).toEqual([]);
});

test('save for next load and apply are separate; Hot policy remains intact', async ({ page, runtime }) => {
  await openModel(page);
  await page.getByLabel('Total context tokens', { exact: true }).fill('16384');
  await page.getByRole('button', { name: 'Save for next load' }).click();
  await expect(page.getByText('Settings saved for the next load.', { exact: false })).toBeVisible();
  await expect.poll(() => runtime.saved.ctx_size).toBe(16384);
  expect(runtime.writes.some((w: any) => w.path === 'juiced/operation')).toBe(false);
  await page.getByRole('button', { name: 'Apply saved settings' }).click();
  expect(runtime.writes.at(-1)).toEqual({ path: 'juiced/operation', body: { model: modelId, action: 'reload' } });
  expect(runtime.models[0].policy).toBe('hot');
});

test('concurrent edit requires review and retains both independent changes', async ({ page, runtime }) => {
  await openModel(page);
  await page.getByLabel('Total context tokens', { exact: true }).fill('16384');
  runtime.saved.ctx_size = 12288;
  runtime.saved.auto_evict = true;
  await page.getByRole('button', { name: 'Save for next load' }).click();
  await expect(page.getByRole('alert')).toContainText('another client');
  expect(runtime.writes).toEqual([]);
  await page.getByRole('button', { name: 'Review latest settings' }).click();
  await expect(page.getByText('Latest saved settings loaded.', { exact: false })).toBeVisible();
  await page.getByRole('button', { name: 'Save for next load' }).click();
  await expect.poll(() => runtime.saved.ctx_size).toBe(16384);
  expect(runtime.saved.auto_evict).toBe(true);
});

test('settings failure recovers in place; README cannot execute HTML', async ({ page, runtime }) => {
  runtime.failOptions = true;
  await openModel(page);
  await expect(page.getByRole('alert')).toContainText('unavailable');
  runtime.failOptions = false;
  await page.getByRole('button', { name: 'Retry settings' }).click();
  await expect(page.getByLabel('Total context tokens', { exact: true })).toBeVisible();
  await page.getByRole('button', { name: 'README', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Ornith model card' })).toBeVisible();
  expect(await page.evaluate(() => (window as any).compromised)).toBeUndefined();
});

test('disabled quant is inspectable but cannot be loaded by settings footer', async ({ page, runtime }) => {
  await openModel(page, otherId);
  await expect(page.getByLabel('Loading policy', { exact: true })).toHaveValue('disabled');
  await expect(page.getByRole('button', { name: 'Load model' })).toBeDisabled();
  expect(runtime.writes).toEqual([]);
});

test('backend readiness, update guard and server-owned installation', async ({ page, runtime }) => {
  await page.goto('/');
  await page.getByRole('button', { name: 'Backends & hardware', exact: true }).click();
  await expect(page.getByRole('button', { name: 'Update backend' })).toBeDisabled();
  await page.getByRole('button', { name: 'Install backend' }).click();
  await expect(page.getByRole('status').filter({ hasText: 'managed by the server' })).toBeVisible();
  expect(runtime.writes.at(-1)).toEqual({ path: 'install', body: { recipe: 'llamacpp', backend: 'vulkan', force: false, stream: true, subscribe: false } });
});

test('completed downloads offer review, not a failed load or automatic enable', async ({ page, runtime }) => {
  await page.goto('/');
  await page.getByRole('button', { name: /^Downloads/ }).click();
  await page.getByRole('button', { name: 'Review model', exact: true }).click();
  await expect(page.getByRole('heading', { name: otherId, exact: true })).toBeVisible();
  expect(runtime.writes).toEqual([]);
});

test('requests filter, pause, failed poll and retry; logs stay available', async ({ page, runtime }) => {
  await page.goto('/');
  await page.getByRole('button', { name: 'Diagnostics', exact: true }).click();
  await expect(page.getByRole('img', { name: /CPU utilization history/ })).toBeVisible();
  await page.getByRole('button', { name: 'Requests', exact: true }).click();
  await expect(page.getByRole('cell', { name: '200', exact: true })).toBeVisible();
  await page.getByLabel('Request status', { exact: true }).selectOption('errors');
  await expect(page.getByRole('cell', { name: '200', exact: true })).toHaveCount(0);
  await expect(page.getByRole('cell', { name: '400', exact: true })).toBeVisible();
  await page.getByRole('button', { name: 'Pause requests' }).click();
  await expect(page.getByLabel('Refresh request history')).toBeDisabled();
  runtime.failRequests = true;
  await page.getByRole('button', { name: 'Resume requests' }).click();
  await expect(page.getByRole('alert')).toContainText('unavailable');
  runtime.failRequests = false;
  await page.getByRole('button', { name: 'Retry requests' }).click();
  await expect(page.getByRole('alert')).toHaveCount(0);
  await page.getByRole('button', { name: 'Logs', exact: true }).click();
  await expect(page.getByRole('button', { name: 'Raw', exact: true })).toBeVisible();
});

for (const width of [1440, 900, 390, 320]) test(`inspector and diagnostics fit ${width}px with independent scrolling`, async ({ page, runtime }) => {
  await page.setViewportSize({ width, height: 800 });
  await openModel(page);
  const list = page.locator('.workspace-primary');
  if (width > 1100) await expect(list).toBeVisible(); else await expect(list).toBeHidden();
  await page.getByRole('button', { name: 'Effective', exact: true }).click();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await page.screenshot({ path: `test-results/inspector-${width}.png`, fullPage: true });
  await page.getByLabel('Back to model list').click();
  if (width < 768) await page.getByLabel('Toggle navigation').click();
  await page.getByRole('button', { name: 'Diagnostics', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Diagnostics', exact: true })).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await page.screenshot({ path: `test-results/diagnostics-${width}.png`, fullPage: true });
});

test('discarding a draft between management screens actually resets the retained inspector', async ({ page, runtime }) => {
  await openModel(page);
  await page.getByLabel('Total context tokens', { exact: true }).fill('16384');
  await page.getByRole('button', { name: 'Library', exact: true }).click();
  await page.getByRole('button', { name: 'Discard changes' }).click();
  await expect(page.getByLabel('Total context tokens', { exact: true })).toHaveValue('8192');
  expect(runtime.writes).toEqual([]);
});

test('Hot removal is protected and invalid values preserve the draft', async ({ page, runtime }) => {
  await openModel(page);
  await page.getByLabel('Total context tokens', { exact: true }).fill('0');
  await page.getByRole('button', { name: 'Save for next load' }).click();
  await expect(page.getByRole('alert')).toBeVisible();
  await expect(page.getByLabel('Total context tokens', { exact: true })).toHaveValue('0');
  expect(runtime.writes).toEqual([]);
  await page.getByRole('button', { name: 'Manage', exact: true }).click();
  await expect(page.getByRole('button', { name: 'Remove model…', exact: true })).toBeDisabled();
});

test('empty catalog and requests explain their state', async ({ page, runtime }) => {
  runtime.empty = true; runtime.requests = [];
  await page.goto('/');
  await expect(page.getByText('The native model catalog is initializing.', { exact: false })).toBeVisible();
  await page.getByRole('button', { name: 'Diagnostics', exact: true }).click();
  await page.getByRole('button', { name: 'Requests', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'No completed inference requests yet' })).toBeVisible();
});

test('temporary service test keeps composer visible and does not persist input', async ({ page, runtime }) => {
  await page.goto('/');
  await page.getByRole('button', { name: 'Test services', exact: true }).click();
  const message = page.getByLabel('Message', { exact: true });
  await message.fill('Temporary UI acceptance draft');
  const bounds = await message.boundingBox();
  expect(bounds!.y + bounds!.height).toBeLessThan(800);
  await page.reload();
  await expect(message).toHaveValue('');
  expect(await page.evaluate(() => JSON.stringify(localStorage))).not.toContain('Temporary UI acceptance draft');
});

test('larger text, visible focus and text contrast remain usable', async ({ page, runtime }) => {
  await page.setViewportSize({ width: 1280, height: 900 });
  await openModel(page);
  await page.evaluate(() => document.documentElement.style.zoom = '1.5');
  await page.getByLabel('Back to model list').focus();
  await page.keyboard.press('Tab');
  const result = await page.evaluate(() => {
    const current = document.activeElement as HTMLElement;
    const root = getComputedStyle(document.documentElement);
    function luminance(hex: string) { const parts = hex.trim().replace('#','').match(/../g)!.map(v => parseInt(v,16) / 255).map(v => v <= .04045 ? v/12.92 : ((v+.055)/1.055)**2.4); return parts[0]*.2126 + parts[1]*.7152 + parts[2]*.0722; }
    const fg = luminance(root.getPropertyValue('--muted-foreground')), bg = luminance(root.getPropertyValue('--background'));
    return { overflow: document.documentElement.scrollWidth > innerWidth, contrast: (fg+.05)/(bg+.05), focused: getComputedStyle(current).outlineStyle };
  });
  expect(result.overflow).toBe(false);
  expect(result.contrast).toBeGreaterThanOrEqual(4.5);
  expect(result.focused).not.toBe('none');
});
