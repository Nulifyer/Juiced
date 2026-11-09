import { test, expect, openModel } from './fixture';
test('hybrid changes require explicit review and unavailable vLLM explains why', async ({ page, runtime }) => {
  await openModel(page); await page.getByRole('button', { name: 'Runtime', exact: true }).click();
  await page.getByRole('combobox', { name: 'Run on', exact: true }).click(); await page.getByRole('option', { name: /Hybrid NPU/ }).click();
  await expect(page.getByText('Partial prefill only:', { exact: false })).toBeVisible();
  const apply=page.getByRole('button', { name: 'Apply & reload' }); await expect(apply).toBeDisabled();
  await page.getByRole('checkbox', { name: 'I’ve reviewed these changes' }).check(); await apply.click();
  expect(runtime.writes.find((w: any) => w.path==='juiced/execution').body).toMatchObject({ profile: 'llamacpp:hybrid-rocm', acknowledge: true });
  await page.locator('.execution-profiles').getByRole('button', { name: 'Refresh', exact: true }).click();
  await expect(page.getByLabel('Current worker activity')).toContainText('48');
  await page.getByRole('combobox', { name: 'Runtime', exact: true }).click(); await page.getByRole('option', { name: 'vLLM', exact: true }).click();
  await expect(page.getByText('The pinned vLLM GGUF importer does not support this architecture')).toBeVisible(); await expect(apply).toBeDisabled();
});
test('runtime profiles offer published variants without producer controls', async ({ page, runtime }) => {
  await openModel(page); await page.getByRole('button', { name: 'Runtime', exact: true }).click();
  await expect(page.getByRole('combobox', { name: 'Runtime', exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Prepare GGUF' })).toHaveCount(0);
  await expect(page.getByRole('region', { name: 'Prepare a variant' })).toHaveCount(0);
  expect(runtime.writes).toEqual([]);
});
test('retained conversion records keep inspection and cancellation without new dispatch', async ({ page, runtime }) => {
  runtime.preparationJobs.old = {id:'old',model:'Ornith-1.5-35B-A3B-GGUF-Q6_K',name:'Retained variant',pending:true,phase:'cleanup-pending'};
  await openModel(page); await page.getByRole('button', { name: 'Runtime', exact: true }).click();
  await expect(page.getByRole('region',{name:'Previous conversion jobs'})).toContainText('cleanup-pending');
  if (process.env.JUICED_DESIGN_CAPTURES) {
    const { mkdir } = await import('node:fs/promises');
    await mkdir(process.env.JUICED_DESIGN_CAPTURES, { recursive: true });
    for (const viewport of [{width:1440,height:1000},{width:390,height:844}]) {
      await page.setViewportSize(viewport);
      await page.getByRole('region',{name:'Previous conversion jobs'}).scrollIntoViewIfNeeded();
      expect(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth)).toBe(false);
      await page.screenshot({path:`${process.env.JUICED_DESIGN_CAPTURES}/retained-history-${viewport.width}.png`,fullPage:true});
    }
  }
  await page.getByRole('button',{name:'Cancel',exact:true}).click();
  await expect(page.getByRole('region',{name:'Previous conversion jobs'})).toContainText('cancelled');
  expect(runtime.writes.map((entry:any)=>entry.body)).toEqual([{action:'cancel',id:'old'}]);
});
test('published variant search keeps the requested runtime and source name', async ({ page, runtime }) => {
  await openModel(page); await page.getByRole('button', { name: 'Runtime', exact: true }).click();
  await page.getByRole('combobox', { name: 'Runtime', exact: true }).click(); await page.getByRole('option', { name: 'vLLM', exact: true }).click();
  await page.getByRole('button', { name: 'Find a published variant' }).click();
  await expect(page.getByRole('textbox', { name: 'Search Hugging Face models' })).toHaveValue(/Ornith/);
  await expect(page.getByRole('combobox', { name: 'Search target runtime' })).toHaveValue('vllm');
});

for (const width of [1440,390]) test(`runtime controls remain compact at ${width}px`, async ({ page, runtime }) => {
  runtime.executionSelected = 'llamacpp:hybrid-rocm';
  await page.setViewportSize({width,height:960}); await openModel(page); await page.getByRole('button',{name:'Runtime',exact:true}).click();
  await page.getByRole('combobox',{name:'Run on',exact:true}).click();await page.getByRole('option',{name:/Hybrid NPU/}).click();
  await expect(page.getByRole('checkbox',{name:'I’ve reviewed these changes'})).toBeVisible();
  expect(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth)).toBe(false);
  await page.screenshot({path:`test-results/runtime-profile-${width}.png`});
  expect(runtime.writes).toEqual([]);
});

test('prepared copy storage protects referenced weights and confirms reclaim',async({page,runtime})=>{
  await page.goto('/');await page.getByRole('button',{name:'Cache',exact:true}).click();await page.getByRole('button',{name:'Retained weights',exact:true}).click();
  await expect(page.getByRole('region',{name:'Retained weights'})).toContainText('In use');
  await expect(page.getByRole('button',{name:'Reclaim space'})).toHaveCount(1);
  await page.getByRole('button',{name:'Reclaim space'}).click();expect(runtime.writes).toEqual([]);
  await page.getByRole('button',{name:'Delete cached weights'}).click();
  await expect(page.getByRole('button',{name:'Reclaim space'})).toHaveCount(0);
  expect(runtime.writes.find((w:any)=>w.path==='juiced/preparation').body).toMatchObject({action:'delete_cache',id:'b'.repeat(64)});
});
