import { test, expect } from '@playwright/test';
test.describe.configure({ mode: 'serial' });
// This mutates only the disposable CPU fixture, never a homelab model.
test('actual container: save, reload, infer, inspect request metadata and restore', async ({ page, request, baseURL }) => {
  test.setTimeout(120_000);
  expect(baseURL).toMatch(/^http:\/\/127\.0\.0\.1:18080$/);
  const model = 'Stories15M-Q4_0';
  const get = async (path: string) => { const response = await request.get('/v1/' + path); expect(response.ok()).toBeTruthy(); return response.json(); };
  const post = async (path: string, data: any) => { const response = await request.post('/v1/' + path, { data }); expect(response.ok()).toBeTruthy(); return response.json(); };
  const catalogModel = async () => (await get('juiced/catalog')).data.find((entry: any) => entry.id === model);
  const original = await get(`models/${model}/options`);
  const before = await catalogModel();
  expect(before.loaded).toBe(true);
  await page.goto('/');
  await page.evaluate(() => localStorage.setItem('juiced.screen', 'models'));
  await page.reload();
  await page.getByRole('button', { name: model, exact: true }).first().click();
  const nextContext = original.effective.ctx_size === 512 ? 1024 : 512;
  try {
    await page.getByLabel('Total context tokens', { exact: true }).fill(String(nextContext));
    await page.getByRole('button', { name: 'Save for next load' }).click();
    await expect(page.getByText('Settings saved for the next load.', { exact: false })).toBeVisible();
    expect((await catalogModel()).runtime.pid).toBe(before.runtime.pid);
    expect((await get(`models/${model}/options`)).effective.ctx_size).toBe(nextContext);
    await page.getByRole('button', { name: 'Apply saved settings' }).click();
    await expect.poll(async () => { const current = await catalogModel(); return current.loaded && current.runtime?.pid > 0 && current.runtime.pid !== before.runtime.pid && !current.operation?.pending; }, { timeout: 60_000 }).toBe(true);
    await page.getByRole('button', { name: 'Effective', exact: true }).click();
    await expect(page.locator('.running-command pre')).toContainText('llama-server');
    const answer = await post('chat/completions', { model, messages: [{ role: 'user', content: 'Once upon a time' }], max_tokens: 8, stream: false });
    expect(answer.choices[0].message.content.length).toBeGreaterThan(0);
    await page.getByLabel('Back to model list').click();
    await page.getByRole('button', { name: 'Diagnostics', exact: true }).click();
    await page.getByRole('button', { name: 'Requests', exact: true }).click();
    await expect(page.getByRole('cell').filter({ hasText: model }).first()).toBeVisible();
    const history = await get('juiced/requests');
    const record = history.data.find((entry: any) => entry.model === model && entry.endpoint === 'chat/completions' && entry.status === 200 && !entry.stream);
    expect(record, JSON.stringify(history)).toBeDefined();
    expect(record.input_tokens, JSON.stringify({ usage: answer.usage, record })).toBeGreaterThan(0);
    expect(record.output_tokens).toBeGreaterThan(0);
    expect(JSON.stringify(history)).not.toContain('Once upon a time');
    await page.screenshot({ path: 'test-results/native-requests.png', fullPage: true });
  } finally {
    await post(`models/${model}/options`, { ctx_size: original.saved.ctx_size ?? null });
    await post('juiced/operation', { model, action: 'reload' });
  }
});

test('actual container: runtime selection reloads and UI preparation creates a Disabled variant', async ({ page, request, baseURL }) => {
  test.setTimeout(180_000);
  expect(baseURL).toBe('http://127.0.0.1:18080');
  const model = 'Stories15M-Q4_0', variant = 'Stories15M-Q6_K';
  const get = async (path: string) => { const response = await request.get('/v1/' + path); expect(response.ok()).toBeTruthy(); return response.json(); };
  const current = async () => (await get('juiced/catalog')).data.find((entry: any) => entry.id === model);
  await expect.poll(async () => { const value = await current(); return value.loaded && !value.operation?.pending; }).toBe(true);
  const original = await get('juiced/execution?model=' + model);
  await page.goto('/'); await page.evaluate(() => localStorage.setItem('juiced.screen', 'models')); await page.reload();
  await page.getByRole('button', { name: model, exact: true }).first().click();
  await page.getByRole('button', { name: 'Runtime', exact: true }).click();
  try {
    await page.getByRole('combobox', { name: 'Run on', exact: true }).click();
    await page.getByRole('option', { name: 'CPU', exact: true }).click();
    await page.getByRole('button', { name: 'Apply & reload' }).click();
    await expect.poll(async () => { const value = await current(); return value.execution_profile === 'llamacpp:cpu' && value.loaded && !value.operation?.pending; }, { timeout: 60_000 }).toBe(true);
    await page.getByRole('checkbox', { name: 'I accept the precision changes and separate disk use' }).check();
    await page.getByRole('button', { name: 'Prepare GGUF' }).click();
    await expect.poll(async () => (await get('juiced/catalog')).data.some((entry: any) => entry.id === variant && entry.downloaded && !entry.loaded && entry.policy === 'disabled'), { timeout: 60_000 }).toBe(true);
    await expect(page.getByRole('region', { name: 'Prepare a variant' })).toContainText('complete', { timeout: 10_000 });
    expect((await get('models')).data.some((entry: any) => entry.id === variant)).toBe(false);
    await page.screenshot({ path: 'test-results/native-runtime-preparation.png', fullPage: true });
  } finally {
    const response = await request.post('/v1/juiced/execution', { data: { model, profile: original.selected, reload: true, acknowledge: true } });
    expect(response.ok()).toBeTruthy();
    await expect.poll(async () => { const value = await current(); return value.loaded && value.execution_profile === original.selected && !value.operation?.pending; }, { timeout: 60_000 }).toBe(true);
  }
});
