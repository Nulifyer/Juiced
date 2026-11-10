import { test, expect, modelId, otherId, openModel } from './fixture';

function assessment(id = modelId, fit = 'fits') {
  return { model: id, enabled: id === modelId, context_tokens: 8192,
    compatibility: { compatible: true, reason: 'Declared compatible runtime' },
    resources: { schema_version: 1, fit, reason: fit === 'unknown' ? 'No execution-specific resource ceilings are recorded' : fit === 'limited_fit' ? 'Declared ceilings fit, but current reservations or observed headroom do not' : 'Declared ceilings fit the sampled capacity',
      observed_at_ms: Date.now(), admission_open: true, estimate_source: 'Controlled fixture ceiling',
      pools: fit === 'unknown' ? {} : { host: { required_peak_bytes: 8 * 1024 ** 3, observed_available_bytes: (fit === 'limited_fit' ? 6 : 16) * 1024 ** 3,
        capacity_bytes: 24 * 1024 ** 3, reserved_bytes: (fit === 'limited_fit' ? 18 : 2) * 1024 ** 3,
        protected_headroom_bytes: 2 * 1024 ** 3, source: 'One shared CPU/GPU pool' } } } };
}

test('host suitability is a read-only saved-settings check with separate memory evidence', async ({ page, runtime }) => {
  let queries = 0;
  await page.route('**/v1/juiced/suitability**', route => {
    expect(route.request().method()).toBe('GET'); queries++;
    return route.fulfill({ json: assessment() });
  });
  await openModel(page);
  expect(queries).toBe(0);
  const region = page.getByRole('region', { name: 'Host fit', exact: true });
  await region.getByRole('button', { name: 'Check saved settings' }).click();
  await expect(region).toContainText('Fits declared limits');
  await expect(region).toContainText('8,192 tokens per request');
  await region.getByText('Memory details', { exact: true }).click();
  await expect(region).toContainText('8.0 GiB');
  await expect(region).toContainText('One shared CPU/GPU pool');
  expect(runtime.writes).toEqual([]);
  await page.getByLabel('Total context tokens', { exact: true }).fill('16384');
  await expect(region.getByRole('button', { name: 'Check saved settings' })).toBeDisabled();
  await expect(region).toContainText('Save your changes');
  await expect(region).not.toContainText('Fits declared limits');
  expect(queries).toBe(1);
});

test('an in-flight host check cannot relabel a changed draft', async ({ page, runtime }) => {
  let release!: () => void;
  const delayed = new Promise<void>(resolve => { release = resolve; });
  await page.route('**/v1/juiced/suitability**', async route => {
    await delayed;
    await route.fulfill({ json: assessment() }).catch(() => {});
  });
  await openModel(page);
  const region = page.getByRole('region', { name: 'Host fit', exact: true });
  const started = page.waitForRequest('**/v1/juiced/suitability**');
  await region.getByRole('button', { name: 'Check saved settings' }).click(); await started;
  await page.getByLabel('Total context tokens', { exact: true }).fill('16384');
  release();
  await expect(region).toContainText('Save your changes');
  await expect(region).not.toContainText('Fits declared limits');
  expect(runtime.writes).toEqual([]);
});

test('host-check failure can retry without losing model settings or enabling a disabled model', async ({ page, runtime }) => {
  let attempts = 0;
  await page.route('**/v1/juiced/suitability**', route => {
    attempts++;
    return route.fulfill(attempts === 1 ? { status: 503, json: { error: 'Capacity probe unavailable' } } : { json: assessment(otherId, 'unknown') });
  });
  await openModel(page, otherId);
  const region = page.getByRole('region', { name: 'Host fit', exact: true });
  await region.getByRole('button', { name: 'Check saved settings' }).click();
  await expect(region.getByRole('alert')).toContainText('Capacity probe unavailable');
  await region.getByRole('button', { name: 'Retry host check' }).click();
  await expect(region).toContainText('Host fit is unknown');
  await expect(region).toContainText('loading policy is Disabled');
  await expect(page.getByRole('combobox', { name: 'Loading policy', exact: true })).toHaveValue('disabled');
  expect(runtime.writes).toEqual([]);
});

for (const width of [1440, 768, 390]) test(`host fit remains readable at ${width}px`, async ({ page, runtime }) => {
  await page.setViewportSize({ width, height: 960 });
  await page.route('**/v1/juiced/suitability**', route => route.fulfill({ json: assessment(modelId, 'limited_fit') }));
  await openModel(page);
  const region = page.getByRole('region', { name: 'Host fit', exact: true });
  await region.getByRole('button', { name: 'Check saved settings' }).click();
  await expect(region).toContainText('Current capacity is in use');
  await region.getByText('Memory details', { exact: true }).click();
  await expect(region.getByRole('button', { name: 'Check saved settings' })).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
  await page.screenshot({ path: `test-results/host-fit-${width}.png`, fullPage: true });
  expect(runtime.writes).toEqual([]);
});
