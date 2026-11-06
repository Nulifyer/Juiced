import { test, expect } from '@playwright/test';
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
    const record = history.data.find((entry: any) => entry.model === model && entry.status === 200 && !entry.stream);
    expect(record.input_tokens).toBeGreaterThan(0);
    expect(record.output_tokens).toBeGreaterThan(0);
    expect(JSON.stringify(history)).not.toContain('Once upon a time');
    await page.screenshot({ path: 'test-results/native-requests.png', fullPage: true });
  } finally {
    await post(`models/${model}/options`, { ctx_size: original.saved.ctx_size ?? null });
    await post('juiced/operation', { model, action: 'reload' });
  }
});
