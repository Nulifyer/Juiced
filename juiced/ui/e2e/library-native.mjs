import { readFile, writeFile } from 'node:fs/promises';
import { chromium, expect as baseExpect } from '@playwright/test';

const fixture = JSON.parse(await readFile(process.argv[2], 'utf8'));
const expect = baseExpect.configure({ timeout: 20_000 });
const browser = await chromium.launch({ executablePath: process.env.JUICED_TEST_CHROMIUM || undefined });
const proof = { passed: false, scope: 'Actual native library jobs and tiny CPU service; no mocked application routes', checks: [] };
const contexts = [];
const errors = [];
const check = (label) => { proof.checks.push(label); console.log(label); };
async function control(body) {
  const response = await fetch(fixture.control_url + '/fixture/control', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) });
  const result = await response.json();
  expect(response.status, JSON.stringify(result)).toBe(200);
}
async function data(path) {
  const response = await fetch(fixture.url + '/v1/' + path);
  expect(response.status).toBe(200);
  return response.json();
}
async function model() { return (await data('juiced/catalog')).data.find((m) => m.id === fixture.model); }
async function context(viewport, name, hasTouch = false) {
  const context = await browser.newContext({ viewport, hasTouch });
  context.setDefaultTimeout(20_000);
  contexts.push([context, name]);
  await context.tracing.start({ screenshots: true, snapshots: true, sources: true });
  const page = await context.newPage();
  page.on('pageerror', (error) => errors.push(error.message));
  return page;
}
async function library(page) {
  await page.goto(fixture.url);
  const nav = page.getByLabel('Toggle navigation');
  if (await nav.isVisible()) await nav.click();
  await page.getByRole('button', { name: 'Library', exact: true }).click();
}
async function usefulReply(page, name) {
  await expect(page.getByRole('heading', { name: 'Test chat', exact: true })).toBeVisible();
  await expect(page.getByLabel('Loaded model', { exact: true })).toHaveValue(fixture.model);
  await page.getByLabel('Message', { exact: true }).fill('Once upon a time, a small cat lived in a blue house.');
  const response = page.waitForResponse((r) => r.url().endsWith('/v1/chat/completions') && r.request().method() === 'POST');
  await page.getByRole('button', { name: 'Send message', exact: true }).click();
  expect((await response).status()).toBe(200);
  await expect(page.getByRole('button', { name: 'Stop generation', exact: true })).toHaveCount(0, { timeout: 45_000 });
  await expect(page.getByRole('button', { name: 'Send message', exact: true })).toBeVisible();
  const paragraph = page.locator('.chat-message.assistant .markdown').last();
  await expect(paragraph).toBeVisible();
  expect((await paragraph.innerText()).trim().length).toBeGreaterThan(0);
  await page.screenshot({ path: fixture.out + '/' + name + '-reply.png' });
  check(name + ': useful response rendered from the selected real CPU worker');
}
try {
  const page = await context({ width: 1440, height: 1000 }, 'desktop');
  await library(page);
  const query = page.getByRole('textbox', { name: 'Search models', exact: true });
  await query.fill('Library Story');
  await page.getByRole('button', { name: 'Search published models', exact: true }).click();
  const repo = page.locator('.published-repository').filter({ hasText: fixture.repo });
  await expect(repo).toBeVisible(); await repo.getByRole('button', { name: 'Choose files', exact: true }).click();
  const review = page.getByRole('dialog');
  await expect(review.getByRole('radio', { checked: true })).toHaveCount(0);
  await expect(review.getByRole('button', { name: 'Choose a variant first', exact: true })).toBeDisabled();
  await review.getByRole('radio', { name: fixture.variant, exact: false }).check();
  await review.getByLabel('Registration name', { exact: true }).fill(fixture.model);
  await page.screenshot({ path: fixture.out + '/desktop-review.png' });
  await review.getByRole('button', { name: 'Download ' + fixture.variant, exact: true }).click();
  await expect(review).toHaveCount(0); await expect(query).toHaveValue('Library Story');
  const transfer = page.locator('.transfer-row').filter({ hasText: fixture.model });
  await expect(transfer.locator('.inline-error')).toBeVisible({ timeout: 15_000 });
  expect((await model()).policy).toBe('disabled'); expect((await model()).loaded).toBe(false);
  check('desktop: real transfer failure stays visible with the model disabled');
  await page.screenshot({ path: fixture.out + '/desktop-download-failure.png' });
  await control({ action: 'restart' });
  await expect(async () => { expect((await model())?.policy).toBe('disabled'); }).toPass({ timeout: 20_000 });
  await expect(query).toHaveValue('Library Story');
  await expect(transfer.getByRole('button', { name: 'Resume / retry', exact: true })).toBeVisible();
  check('desktop: manager restart retains registration, query and native recovery action');
  await control({ mode: 'retry' });
  await transfer.getByRole('button', { name: 'Resume / retry', exact: true }).click();
  await expect(transfer.getByRole('status')).toContainText('after HTTP 503', { timeout: 15_000 });
  expect(Number(await transfer.locator('progress').getAttribute('value'))).toBeLessThan(100);
  await transfer.getByRole('button', { name: 'Pause', exact: true }).click();
  await expect(transfer.getByRole('button', { name: 'Resume / retry', exact: true })).toBeVisible({ timeout: 5_000 });
  check('desktop: HTTP 503 retries stay below complete and pause without waiting for backoff');
  await control({ mode: 'hold' });
  await transfer.getByRole('button', { name: 'Resume / retry', exact: true }).click();
  await expect(transfer.getByRole('button', { name: 'Pause', exact: true })).toBeVisible();
  await expect.poll(async () => Number(await transfer.locator('progress').getAttribute('value')), { timeout: 30_000 }).toBeGreaterThan(0);
  await transfer.getByRole('button', { name: 'Pause', exact: true }).click();
  await expect(transfer.getByRole('button', { name: 'Resume / retry', exact: true })).toBeVisible();
  check('desktop: accepted native transfer exposes working progress and pause');
  await control({ mode: 'run' });
  await transfer.getByRole('button', { name: 'Resume / retry', exact: true }).click();
  await expect.poll(async () => (await model())?.downloaded, { timeout: 45_000 }).toBe(true);
  await expect(transfer.getByRole('button', { name: 'Review model', exact: true })).toBeVisible();
  expect((await model()).policy).toBe('disabled'); expect((await model()).registry_source).toBe('huggingface');
  check('desktop: resumed real download completes in place without enabling the model');
  await transfer.getByRole('button', { name: 'Review model', exact: true }).click();
  await expect(page.getByRole('button', { name: 'Load model', exact: true })).toBeDisabled();
  await page.getByRole('button', { name: 'Runtime', exact: true }).click();
  await page.getByRole('combobox', { name: 'Run on', exact: true }).click();
  await page.getByRole('option', { name: 'CPU', exact: true }).click();
  await page.getByRole('button', { name: 'Use this profile', exact: true }).click();
  await expect.poll(async () => (await data('juiced/execution?model=' + encodeURIComponent(fixture.model))).selected).toBe('llamacpp:cpu');
  await expect.poll(async () => !!(await model()).operation?.pending).toBe(false);
  await page.getByRole('button', { name: 'Settings', exact: true }).click();
  await page.getByLabel('Total context tokens', { exact: true }).fill('512');
  await page.getByRole('button', { name: 'Save for next load', exact: true }).click();
  await expect.poll(async () => (await data('models/' + fixture.model + '/options')).effective.ctx_size).toBe(512);
  await page.getByLabel('Loading policy', { exact: true }).selectOption('on-demand');
  await expect(page.getByRole('button', { name: 'Load model', exact: true })).toBeEnabled();
  await page.getByRole('button', { name: 'Load model', exact: true }).click();
  await expect.poll(async () => !!(await model()).loaded, { timeout: 45_000 }).toBe(true);
  await expect(page.getByRole('button', { name: 'Test this variant', exact: true })).toBeEnabled();
  await page.getByRole('button', { name: 'Test this variant', exact: true }).click();
  await usefulReply(page, 'desktop');
  await page.getByRole('button', { name: 'Service APIs', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Service APIs', exact: true })).toBeVisible();
  await expect(page.getByLabel('Client model', { exact: true })).toHaveValue(fixture.model);
  await expect(page.getByRole('button', { name: 'Copy model ID', exact: true })).toBeVisible();
  check('desktop: endpoint setup exposes the enabled model after useful service');
  const phone = await context({ width: 390, height: 960 }, 'phone', true);
  await library(phone); await phone.getByRole('textbox', { name: 'Search models', exact: true }).fill(fixture.model);
  const row = phone.locator('.discover-row').filter({ hasText: fixture.model });
  await expect(row).toContainText('Running'); await expect(row).toContainText('On demand');
  expect(await phone.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await phone.screenshot({ path: fixture.out + '/phone-library.png' });
  await row.getByRole('button', { name: 'Try model', exact: true }).tap();
  await usefulReply(phone, 'phone');
  expect(errors).toEqual([]);
  proof.unhandled_browser_errors = errors;
  proof.passed = true;
} finally {
  for (const [context, name] of contexts) {
    await context.tracing.stop({ path: fixture.out + '/' + name + '-trace.zip' });
    await context.close();
  }
  await browser.close();
  await writeFile(fixture.out + '/browser-proof.json', JSON.stringify(proof, null, 2) + '\n');
}
