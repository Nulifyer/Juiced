import { test, expect, openModel, otherId } from './fixture';
test('hybrid changes require explicit review and unavailable vLLM explains why', async ({ page, runtime }) => {
  await openModel(page); await page.getByRole('button', { name: 'Runtime', exact: true }).click();
  await page.getByRole('combobox', { name: 'Run on', exact: true }).click(); await page.getByRole('option', { name: /Hybrid NPU/ }).click();
  await expect(page.getByText('Partial prefill only:', { exact: false })).toBeVisible();
  const apply=page.getByRole('button', { name: 'Apply & reload' }); await expect(apply).toBeDisabled();
  await page.getByRole('checkbox', { name: 'I’ve reviewed these changes' }).check(); await apply.click();
  expect(runtime.writes.find((w: any) => w.path==='juiced/execution').body).toMatchObject({ profile: 'llamacpp:hybrid-rocm', acknowledge: true });
  await page.getByRole('combobox', { name: 'Runtime', exact: true }).click(); await page.getByRole('option', { name: 'vLLM', exact: true }).click();
  await expect(page.getByText('The pinned vLLM GGUF importer does not support this architecture')).toBeVisible(); await expect(apply).toBeDisabled();
});
test('preparation shows numerical changes, progress and cancellation', async ({ page, runtime }) => {
  await openModel(page); await page.getByRole('button', { name: 'Runtime', exact: true }).click();
  const prepare=page.getByRole('button', { name: 'Prepare GGUF' }); await expect(prepare).toBeDisabled();
  await page.getByRole('combobox', { name: 'Preparation quantization' }).click(); await page.getByRole('option', { name: 'Q5_K_M', exact: true }).click();
  await page.getByRole('checkbox', { name: 'I accept the precision changes and separate disk use' }).check(); await prepare.click();
  await expect(page.getByRole('region', { name: 'Prepare a variant' })).toContainText('quantizing');
  expect(runtime.writes.find((w: any) => w.path==='juiced/preparation').body).toMatchObject({ quant: 'Q5_K_M', acknowledge: true });
  await page.getByRole('button', { name: 'Cancel', exact: true }).click();
  await expect(page.getByRole('region', { name: 'Prepare a variant' })).toContainText('cancelled');
});
test('prepared model search keeps the requested runtime and source name', async ({ page, runtime }) => {
  await openModel(page); await page.getByRole('button', { name: 'Runtime', exact: true }).click();
  await page.getByRole('combobox', { name: 'Runtime', exact: true }).click(); await page.getByRole('option', { name: 'vLLM', exact: true }).click();
  await page.getByRole('button', { name: 'Find a prepared version' }).click();
  await expect(page.getByRole('textbox', { name: 'Search Hugging Face models' })).toHaveValue(/Ornith/);
  await expect(page.getByRole('combobox', { name: 'Search target runtime' })).toHaveValue('vllm');
});

for (const width of [1440,390]) test(`runtime controls remain compact at ${width}px`, async ({ page, runtime }) => {
  await page.setViewportSize({width,height:960}); await openModel(page); await page.getByRole('button',{name:'Runtime',exact:true}).click();
  await page.getByRole('combobox',{name:'Run on',exact:true}).click();await page.getByRole('option',{name:/Hybrid NPU/}).click();
  await expect(page.getByRole('checkbox',{name:'I’ve reviewed these changes'})).toBeVisible();
  expect(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth)).toBe(false);
  await page.screenshot({path:`test-results/runtime-profile-${width}.png`});
  expect(runtime.writes).toEqual([]);
});

test('prepared copy storage protects referenced weights and confirms reclaim',async({page,runtime})=>{
  await page.goto('/');await page.getByRole('button',{name:'Cache',exact:true}).click();await page.getByRole('button',{name:'Prepared copies',exact:true}).click();
  await expect(page.getByRole('region',{name:'Prepared copies'})).toContainText('In use');
  await expect(page.getByRole('button',{name:'Reclaim space'})).toHaveCount(1);
  await page.getByRole('button',{name:'Reclaim space'}).click();expect(runtime.writes).toEqual([]);
  await page.getByRole('button',{name:'Delete cached weights'}).click();
  await expect(page.getByRole('button',{name:'Reclaim space'})).toHaveCount(0);
  expect(runtime.writes.find((w:any)=>w.path==='juiced/preparation').body).toMatchObject({action:'delete_cache',id:'b'.repeat(64)});
});

test('an available quant opens its existing registration without preparing or enabling it',async({page,runtime})=>{
  runtime.existingPrepared=true;await openModel(page);await page.getByRole('button',{name:'Runtime',exact:true}).click();
  await page.getByRole('combobox',{name:'Preparation quantization'}).click();await page.getByRole('option',{name:/Q4_K_M/}).click();
  await page.getByRole('button',{name:'Open existing variant'}).click();
  await expect(page.getByRole('region',{name:'Model details',exact:true}).getByRole('heading',{name:otherId,exact:true})).toBeVisible();
  expect(runtime.writes).toEqual([]);
});
