import { test, expect, otherId } from './fixture';
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
const searchButton = (page: any) => page.getByRole('button', { name: 'Search published models', exact: true });

test('one library leads with host models and keeps catalog picks separate from residency', async ({ page, runtime }) => {
  seed(runtime); await library(page);
  const rows = page.locator('.discover-row');
  await expect(rows).toHaveCount(7);
  await expect(rows.first()).toContainText('Running');
  await expect(page.getByRole('textbox', { name: 'Search models', exact: true })).toHaveCount(1);
  await expect(page.getByRole('button', { name: 'Downloaded', exact: true })).toHaveCount(0);
  await page.getByText('Filter known models', { exact: true }).click();
  await page.getByRole('button', { name: 'Catalog picks', exact: true }).click();
  await expect(rows).toHaveCount(1); await expect(rows).toContainText('Gemma-4-26B-GGUF');
  expect(runtime.models.find((m: any) => m.id === 'Gemma-4-26B-GGUF').policy).toBe('disabled');
  expect(runtime.writes).toEqual([]);
});

test('shared query and known-model filters compose without another inventory search', async ({ page, runtime }) => {
  seed(runtime); await library(page);
  const rows = page.locator('.discover-row'), input = page.getByLabel('Search models', { exact: true });
  await input.fill('Gemma');
  await page.getByRole('button', { name: 'Clear model search', exact: true }).click();
  await expect(input).toBeFocused();
  await page.getByText('Filter known models', { exact: true }).click();
  await page.getByLabel('Filter by service', { exact: true }).selectOption('Speak');
  await expect(rows).toHaveCount(1); await expect(rows).toContainText('Kokoro-v1');
  await page.getByRole('button', { name: 'Catalog picks', exact: true }).click();
  await expect(rows).toHaveCount(0);
  await expect(searchButton(page)).toBeVisible();
  await page.getByRole('button', { name: 'Reset filters', exact: true }).click();
  await input.fill('qwen coding');
  await page.getByLabel('Filter by capability', { exact: true }).selectOption('coding');
  await expect(rows).toHaveCount(1); await expect(rows).toContainText('Qwen-Coder');
  await page.getByLabel('Filter by runtime', { exact: true }).selectOption('whispercpp');
  await expect(page.getByRole('heading', { name: 'No known models match' })).toBeVisible();
  await page.getByRole('button', { name: 'Reset filters', exact: true }).click();
  await expect(input).toHaveValue('qwen coding');
  await page.getByRole('button', { name: 'Clear model search', exact: true }).click();
  await page.getByLabel('Filter by download size', { exact: true }).selectOption('2');
  await expect(rows).toHaveCount(2);
  await page.getByLabel('Filter by availability', { exact: true }).selectOption('yes');
  await expect(rows).toHaveCount(0);
  await page.getByRole('button', { name: 'Reset filters', exact: true }).click();
  await page.getByLabel('Sort models', { exact: true }).selectOption('smallest');
  await expect(rows.first()).toContainText('Whisper-Tiny');
  expect(runtime.writes).toEqual([]);
});

test('reviewed catalog download stays in the query and never silently enables the model', async ({ page, runtime }) => {
  seed(runtime); await library(page);
  const input = page.getByLabel('Search models', { exact: true }); await input.fill('Gemma');
  await page.locator('.discover-row').getByRole('button', { name: 'Review download', exact: true }).click();
  const review = page.getByRole('dialog', { name: 'Review download', exact: true });
  await expect(review).toContainText('Publisher/Gemma-4-26B-GGUF:Q4_K_M');
  await expect(review).toContainText('Disabled'); expect(runtime.writes).toEqual([]);
  await review.getByRole('button', { name: 'Download model', exact: true }).click();
  await expect(review).toHaveCount(0);
  await expect(page.getByRole('heading', { name: 'Model library', exact: true })).toBeVisible();
  await expect(input).toHaveValue('Gemma');
  await expect(page.locator('.library-activity')).toHaveJSProperty('open', true);
  expect(runtime.writes).toEqual([{ path: 'pull', body: { model_name: 'Gemma-4-26B-GGUF', stream: true, subscribe: false } }]);
  expect(runtime.models.find((m: any) => m.id === 'Gemma-4-26B-GGUF').policy).toBe('disabled');
});

test('published search recovers in the same query, filters publishers and rejects stale results', async ({ page, runtime }) => {
  seed(runtime); let fail = true;
  await page.route('**/v1/registry/search?**', async route => {
    const q = new URL(route.request().url()).searchParams.get('query');
    if (q === 'slow') await new Promise(resolve => setTimeout(resolve, 600));
    if (fail) return route.fulfill({ status: 502, json: { error: 'Search provider unavailable' } });
    return route.fulfill({ json: { results: [{ repository_id: `Alpha/${q}-GGUF` }, { repository_id: `Beta/${q}-GGUF` }] } });
  });
  await library(page);
  const input = page.getByLabel('Search models', { exact: true });
  await input.fill('Ornith'); await searchButton(page).click();
  await expect(page.getByRole('alert')).toContainText('Search provider unavailable');
  await expect(input).toHaveValue('Ornith'); await expect(page.locator('.discover-row')).toHaveCount(2); fail = false;
  await page.getByRole('button', { name: 'Retry search', exact: true }).click();
  await expect(page.locator('.discovery-model')).toHaveCount(2);
  await page.getByLabel('Filter search by publisher').selectOption('Beta');
  await expect(page.locator('.discovery-model')).toHaveCount(1);
  await input.fill('slow'); await searchButton(page).click();
  await input.fill('latest'); await searchButton(page).click();
  await expect(page.locator('.discovery-model').first()).toContainText('latest');
  await page.waitForTimeout(700);
  await expect(page.locator('.discovery-model').first()).toContainText('latest');
  expect(runtime.writes).toEqual([]);
});

for (const width of [1440, 900, 390, 320]) test(`shared search, activity and published files fit ${width}px`, async ({ page, runtime }) => {
  seed(runtime); await page.setViewportSize({ width, height: 960 }); await library(page);
  await expect(page.locator('.discover-row').first()).toBeVisible();
  const heading = await page.locator('.page-heading').boundingBox(), activity = await page.locator('.library-activity').boundingBox();
  expect(activity!.y).toBeGreaterThanOrEqual(heading!.y + heading!.height);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  expect(await page.evaluate(() => document.documentElement.scrollHeight <= innerHeight + 1)).toBe(true);
  await page.screenshot({ path: `${process.env.JUICED_DISCOVERY_CAPTURES || 'test-results'}/discovery-${width}.png` });
  await page.getByLabel('Search models', { exact: true }).fill('Ornith'); await searchButton(page).click();
  await page.getByRole('button', { name: 'Choose files', exact: true }).click();
  const review = page.getByRole('dialog'); await expect(review).toBeVisible();
  await expect(review.getByRole('radio', { checked: true })).toHaveCount(0);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await page.screenshot({ path: `${process.env.JUICED_DISCOVERY_CAPTURES || 'test-results'}/files-${width}.png` });
  expect(runtime.writes).toEqual([]);
});

test('an explicit published quant and projector download their exact files with the original query retained', async ({ page, runtime }) => {
  const repo = 'ornith-ai/Ornith-1.5-35B-A3B-GGUF', file = 'Ornith-1.5-35B-Q8_0.gguf', projector = 'mmproj-Ornith-1.5-35B-BF16.gguf';
  await page.route('**/v1/pull/variants?**', route => route.fulfill({ json: { checkpoint: repo, recipe: 'llamacpp', suggested_name: 'Ornith-1.5-35B-A3B-GGUF', variants: [{ name: file, size_bytes: 35000000000 }], mmproj_files: [projector], suggested_labels: ['tool-calling'] } }));
  await library(page); const query = 'https://huggingface.co/' + repo;
  await page.getByLabel('Search models').fill(query); await searchButton(page).click();
  const review = page.getByRole('dialog'); await expect(review).toBeVisible();
  await expect(review.getByRole('radio', { checked: true })).toHaveCount(0);
  await expect(review.getByRole('button', { name: 'Choose a variant first', exact: true })).toBeDisabled();
  await review.getByRole('radio', { name: new RegExp(file) }).check();
  await review.getByLabel('Vision projector').selectOption(projector);
  await review.getByRole('button', { name: `Download ${file}`, exact: true }).click();
  await expect(page.getByLabel('Search models')).toHaveValue(query);
  expect(runtime.writes).toEqual([{ path: 'pull', body: { model_name: 'user.Ornith-1.5-35B-A3B-GGUF-Q8_0', stream: true, subscribe: false, recipe: 'llamacpp', source: 'huggingface', checkpoints: { main: `${repo}:${file}`, mmproj: `${repo}:${projector}` }, labels: ['tool-calling', 'vision'] } }]);
});

test('catalog details can be reviewed before download without implying it can already run', async ({ page, runtime }) => {
  seed(runtime); await library(page);
  await page.getByRole('button', { name: 'Gemma-4-26B-GGUF', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Gemma-4-26B-GGUF', exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Load model', exact: true })).toBeDisabled();
  expect(runtime.writes).toEqual([]);
});

test('download failure keeps its declared selection and error in the review for retry', async ({ page, runtime }) => {
  seed(runtime); let attempts = 0;
  await page.route('**/v1/pull', route => ++attempts === 1 ? route.fulfill({ status: 503, json: { error: 'Acquisition unavailable' } }) : route.fallback());
  await library(page); await page.locator('.discover-row').filter({ hasText: 'Gemma-4-26B-GGUF' }).getByRole('button', { name: 'Review download', exact: true }).click();
  const review = page.getByRole('dialog', { name: 'Review download', exact: true });
  await review.getByRole('button', { name: 'Download model', exact: true }).click();
  await expect(review.getByRole('alert')).toContainText('Acquisition unavailable');
  await expect(review).toContainText('Publisher/Gemma');
  await review.getByRole('button', { name: 'Download model', exact: true }).click();
  await expect(review).toHaveCount(0); expect(attempts).toBe(2);
  expect(runtime.writes.map((w: any) => w.path)).toEqual(['pull']);
});

test('retained weights remain searchable when the required runtime is unavailable', async ({ page, runtime }) => {
  runtime.models.push({ ...runtime.models[1], id: 'Retained-F16', available: false, unavailable_reason: 'Runtime is unavailable' });
  await library(page); await page.getByLabel('Search models').fill('Retained');
  const row = page.locator('.discover-row'); await expect(row).toContainText('On this host');
  await row.getByRole('button', { name: 'Review requirements', exact: true }).click();
  await expect(page.getByRole('region', { name: 'Model details', exact: true })).toContainText('Runtime is unavailable');
  await expect(page.getByRole('button', { name: 'Load model', exact: true })).toBeDisabled();
  expect(runtime.writes).toEqual([]);
});

test('downloaded model setup requires explicit policy and a ready worker before testing', async ({ page, runtime }) => {
  await library(page); const row = page.locator('.discover-row').filter({ hasText: otherId });
  await row.getByRole('button', { name: 'Set up & run', exact: true }).click();
  await expect(page.getByRole('button', { name: 'Load model', exact: true })).toBeDisabled();
  expect(runtime.writes).toEqual([]);
  await page.getByLabel('Loading policy', { exact: true }).selectOption('on-demand');
  await expect(page.getByRole('button', { name: 'Load model', exact: true })).toBeEnabled();
  await page.getByRole('button', { name: 'Load model', exact: true }).click();
  await expect.poll(() => runtime.writes.some((w: any) => w.path === 'juiced/operation' && w.body.model === otherId && w.body.action === 'load')).toBe(true);
  await expect(page.getByRole('button', { name: 'Test this variant', exact: true })).toBeDisabled();
  runtime.models[1].loaded = true; runtime.models[1].state = 'loaded';
  await expect(page.getByRole('button', { name: 'Test this variant', exact: true })).toBeEnabled();
  await page.getByRole('button', { name: 'Test this variant', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Test chat', exact: true })).toBeVisible();
  await expect(page.getByLabel('Loaded model', { exact: true })).toHaveValue(otherId);
  await page.getByRole('button', { name: 'Service APIs', exact: true }).click();
  const clientModel = page.getByLabel('Client model', { exact: true });
  await expect(clientModel).toHaveValue(otherId);
  await expect(clientModel.locator('option')).toHaveCount(2);
  await expect(page.getByRole('button', { name: 'Copy model ID', exact: true })).toBeVisible();
});

test('Hugging Face review and download bind the selected registry explicitly', async ({ page, runtime }) => {
  let reviewSource = '';
  await page.route('**/v1/pull/variants?**', route => {
    reviewSource = new URL(route.request().url()).searchParams.get('source') || '';
    return route.fallback();
  });
  await library(page); await page.getByLabel('Search models').fill('Example/Ornith-GGUF');
  await searchButton(page).click(); await expect(page.getByRole('dialog')).toBeVisible();
  expect(reviewSource).toBe('huggingface');
  await page.getByRole('radio', { name: /Ornith-Q6_K/ }).check();
  await page.getByRole('button', { name: 'Open installed variant', exact: true }).click();
  expect(runtime.writes).toEqual([]);
});

test('ModelScope registration keeps its source through review, failure and retry', async ({ page, runtime }) => {
  runtime.models[1].registry_source = 'modelscope';
  let fail = true; const sources: string[] = [];
  await page.route('**/v1/pull/variants?**', route => {
    sources.push(new URL(route.request().url()).searchParams.get('source') || '');
    return fail ? route.fulfill({ status: 503, json: { error: 'Registry unavailable' } }) : route.fulfill({ json: { checkpoint: 'Example/Ornith-GGUF', source: 'modelscope', suggested_name: 'Ornith', variants: [{ name: 'Q4_K_M', files: ['Ornith-Q4_K_M.gguf'] }, { name: 'Q6_K', files: ['Ornith-Q6_K.gguf'] }], mmproj_files: [] } });
  });
  await library(page); await page.locator('.discover-row').filter({ hasText: otherId }).getByRole('button', { name: 'Set up & run', exact: true }).click();
  await page.getByRole('button', { name: 'Add another quant', exact: false }).click();
  await expect(page.getByRole('alert')).toContainText('Registry unavailable'); fail = false;
  await page.getByRole('button', { name: 'Retry file review', exact: true }).click();
  const review = page.getByRole('dialog'); await expect(review).toBeVisible();
  await expect(review.getByRole('link', { name: 'Example/Ornith-GGUF', exact: false })).toHaveAttribute('href', 'https://modelscope.cn/models/Example/Ornith-GGUF');
  expect(sources).toEqual(['modelscope', 'modelscope']);
  await review.getByRole('radio', { name: /Q6_K/ }).check();
  await expect(review.getByRole('button', { name: 'Download Q6_K', exact: true })).toBeEnabled();
  await expect(review.getByRole('button', { name: 'Open installed variant', exact: true })).toHaveCount(0);
  await page.getByRole('button', { name: 'Close file review', exact: true }).click();
  await expect(page.getByRole('textbox', { name: 'Search models', exact: true })).toBeFocused();
  expect(runtime.writes).toEqual([]);
});

test('text-only selection does not advertise vision without its matching projector', async ({ page, runtime }) => {
  await page.route('**/v1/pull/variants?**', route => route.fulfill({ json: { checkpoint: 'Publisher/Multimodal-GGUF', source: 'huggingface', recipe: 'llamacpp', suggested_name: 'Multimodal', variants: [{ name: 'Q4_K_M', files: ['model-Q4_K_M.gguf'] }], mmproj_files: ['mmproj.gguf'], suggested_labels: ['vision', 'tool-calling'] } }));
  await library(page); await page.getByRole('textbox', { name: 'Search models', exact: true }).fill('Publisher/Multimodal-GGUF');
  await searchButton(page).click(); const review = page.getByRole('dialog');
  await expect(review.getByLabel('Vision projector')).toHaveValue('');
  await review.getByRole('radio', { name: /Q4_K_M/ }).check();
  await review.getByRole('button', { name: 'Download Q4_K_M', exact: true }).click();
  expect(runtime.writes[0].body.labels).toEqual(['tool-calling']);
  expect(runtime.writes[0].body.checkpoints).toEqual({ main: 'Publisher/Multimodal-GGUF:Q4_K_M' });
});
