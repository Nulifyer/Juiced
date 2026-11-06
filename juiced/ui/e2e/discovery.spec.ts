import { test, expect } from './fixture';
function seed(runtime: any) {
  const model = (id: string, service: string, recipe: string, size: number, labels: string[]) => ({ id, service, recipe, size, labels, suggested: true, checkpoint: `Publisher/${id}${recipe === "llamacpp" ? ":Q4_K_M" : ""}`, downloaded: false, loaded: false, state: 'unloaded', policy: 'disabled', available: true });
  runtime.models.push(
    model('Gemma-4-26B-GGUF', 'Chat', 'llamacpp', 18, ['hot', 'vision', 'tool-calling']),
    model('Qwen-Coder-7B-GGUF', 'Chat', 'llamacpp', 8, ['coding', 'tool-calling']),
    model('Whisper-Tiny', 'Transcribe', 'whispercpp', .075, ['transcription']),
    model('Kokoro-v1', 'Speak', 'kokoro', .3, ['tts']),
    model('Z-Image-Turbo', 'Images', 'sd-cpp', 8, ['image']),
  );
}
async function library(page: any) {
  await page.goto('/');
  if (await page.getByLabel('Toggle navigation').isVisible()) await page.getByLabel('Toggle navigation').click();
  await page.getByRole('button', { name: 'Library', exact: true }).click();
}

test('catalog starts populated with upstream picks first and independent of residency', async ({ page, runtime }) => {
  seed(runtime); await library(page);
  const rows = page.locator('.discover-row');
  await expect(rows).toHaveCount(7);
  await expect(rows.first()).toContainText('Gemma-4-26B-GGUF');
  await expect(rows.first()).toContainText('Recommended');
  await page.getByRole('button', { name: 'Recommended', exact: true }).click();
  await expect(rows).toHaveCount(1);
  expect(runtime.models.find((m: any) => m.id === 'Gemma-4-26B-GGUF').policy).toBe('disabled');
  expect(runtime.writes).toEqual([]);
});

test('catalog search, service, capability, runtime, size, download and reset filters compose', async ({ page, runtime }) => {
  seed(runtime); await library(page);
  const rows = page.locator('.discover-row');
  await page.getByLabel('Search model catalog', { exact: true }).fill('Gemma');
  await page.getByRole('button', { name: 'Clear catalog search', exact: true }).click();
  await expect(page.getByLabel('Search model catalog', { exact: true })).toBeFocused();
  await page.getByLabel('Filter by service', { exact: true }).selectOption('Speak');
  await expect(rows).toHaveCount(1); await expect(rows).toContainText('Kokoro-v1');
  await page.getByRole('button', { name: 'Recommended', exact: true }).click();
  await expect(rows).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Search Hugging Face', exact: true })).toHaveCount(0);
  await page.getByRole('button', { name: 'Reset filters', exact: true }).click();
  await page.getByLabel('Search model catalog', { exact: true }).fill('qwen coding');
  await page.getByLabel('Filter by capability', { exact: true }).selectOption('coding');
  await expect(rows).toHaveCount(1); await expect(rows).toContainText('Qwen-Coder');
  await page.getByLabel('Filter by runtime', { exact: true }).selectOption('whispercpp');
  await expect(page.getByRole('heading', { name: 'No models match these filters' })).toBeVisible();
  await page.getByRole('button', { name: 'Reset filters', exact: true }).first().click();
  await page.getByLabel('Filter by download size', { exact: true }).selectOption('2');
  await expect(rows).toHaveCount(2);
  await page.getByLabel('Filter by download status', { exact: true }).selectOption('yes');
  await expect(rows).toHaveCount(0);
  await page.getByRole('button', { name: 'Reset filters', exact: true }).first().click();
  await page.getByLabel('Sort model catalog', { exact: true }).selectOption('smallest');
  await expect(rows.first()).toContainText('Whisper-Tiny');
  expect(runtime.writes).toEqual([]);
});

test('catalog download starts the selected registration without enabling or loading it', async ({ page, runtime }) => {
  seed(runtime); await library(page);
  await page.locator('.discover-row').filter({ hasText: 'Gemma-4-26B-GGUF' }).getByRole('button', { name: 'Download', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Downloads', exact: true })).toBeVisible();
  expect(runtime.writes).toEqual([{ path: 'pull', body: { model_name: 'Gemma-4-26B-GGUF', stream: true, subscribe: false } }]);
  expect(runtime.models.find((m: any) => m.id === 'Gemma-4-26B-GGUF').policy).toBe('disabled');
});

test('Hugging Face search recovers, retains input, filters publishers and rejects stale results', async ({ page, runtime }) => {
  seed(runtime); let fail = true;
  await page.route('**/v1/registry/search?**', async route => {
    const q = new URL(route.request().url()).searchParams.get('query');
    if (q === 'slow') await new Promise(resolve => setTimeout(resolve, 600));
    if (fail) return route.fulfill({ status: 502, json: { error: 'Search provider unavailable' } });
    return route.fulfill({ json: { results: [
      { repository_id: `Alpha/${q}-GGUF`, downloads: 5 },
      { repository_id: `Beta/${q}-GGUF`, downloads: 100 },
    ] } });
  });
  await library(page); await page.getByRole('button', { name: 'Hugging Face', exact: true }).click();
  const input = page.getByLabel('Search Hugging Face models', { exact: true });
  await input.fill('Ornith'); await page.getByRole('button', { name: 'Search', exact: true }).click();
  await expect(page.getByRole('alert')).toContainText('Search provider unavailable');
  await expect(input).toHaveValue('Ornith'); fail = false;
  await page.getByRole('button', { name: 'Retry search', exact: true }).click();
  await expect(page.locator('.discovery-model')).toHaveCount(2);
  await page.getByLabel('Filter search by publisher').selectOption('Beta');
  await expect(page.locator('.discovery-model')).toHaveCount(1);
  await input.fill('slow'); await page.getByRole('button', { name: 'Search', exact: true }).click();
  await input.fill('latest'); await page.getByRole('button', { name: 'Search', exact: true }).click();
  await expect(page.locator('.discovery-model').first()).toContainText('latest');
  await page.waitForTimeout(700);
  await expect(page.locator('.discovery-model').first()).toContainText('latest');
  expect(runtime.writes).toEqual([]);
});

for (const width of [1440, 900, 390, 320]) test(`discovery filters, catalog and quants fit ${width}px`, async ({ page, runtime }) => {
  seed(runtime); await page.setViewportSize({ width, height: 960 }); await library(page);
  await expect(page.locator('.discover-row').first()).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  expect(await page.evaluate(() => document.documentElement.scrollHeight <= innerHeight + 1)).toBe(true);
  await page.screenshot({ path: `${process.env.JUICED_DISCOVERY_CAPTURES || 'test-results'}/discovery-${width}.png` });
  await page.getByRole('button', { name: 'Hugging Face', exact: true }).click();
  await page.getByLabel('Search Hugging Face models', { exact: true }).fill('Ornith');
  await page.getByRole('button', { name: 'Search', exact: true }).click();
  await page.getByRole('button', { name: 'Browse quants', exact: true }).click();
  await expect(page.getByRole('dialog')).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  expect(runtime.writes).toEqual([]);
});

test('a new quant downloads its exact files and projector with a proper model name', async ({ page, runtime }) => {
  const repo = 'ornith-ai/Ornith-1.5-35B-A3B-GGUF';
  const file = 'Ornith-1.5-35B-Q8_0.gguf';
  const projector = 'mmproj-Ornith-1.5-35B-BF16.gguf';
  await page.route('**/v1/pull/variants?**', route => route.fulfill({ json: { checkpoint: repo, recipe: 'llamacpp', suggested_name: 'Ornith-1.5-35B-A3B-GGUF', variants: [{ name: file, size_bytes: 35000000000 }], mmproj_files: [projector], suggested_labels: ['tool-calling'] } }));
  await library(page); await page.getByRole('button', { name: 'Hugging Face', exact: true }).click();
  await page.getByLabel('Search Hugging Face models').fill('https://huggingface.co/' + repo);
  await page.getByRole('button', { name: 'Search', exact: true }).click();
  await expect(page.getByRole('dialog')).toBeVisible();
  await page.getByRole('button', { name: `Download ${file}`, exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Downloads', exact: true })).toBeVisible();
  expect(runtime.writes).toEqual([{ path: 'pull', body: {
    model_name: 'user.Ornith-1.5-35B-A3B-GGUF-Q8_0', stream: true, subscribe: false,
    recipe: 'llamacpp', checkpoints: { main: `${repo}:${file}`, mmproj: `${repo}:${projector}` }, labels: ['tool-calling', 'vision'],
  } }]);
});

test('catalog model details can be reviewed before downloading', async ({ page, runtime }) => {
  seed(runtime); await library(page);
  await page.getByRole('button', { name: 'Gemma-4-26B-GGUF', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Gemma-4-26B-GGUF', exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Load model', exact: true })).toBeDisabled();
  expect(runtime.writes).toEqual([]);
});
