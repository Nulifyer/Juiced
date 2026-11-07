import { test, expect, openModel, modelId } from './fixture';
const pixel='iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=';
async function navigate(page: any, name: string) {
  if (await page.getByLabel('Toggle navigation').isVisible() && await page.getByLabel('Toggle navigation').getAttribute('aria-expanded') !== 'true') await page.getByLabel('Toggle navigation').click();
  await page.getByRole('button', { name, exact: true }).first().click();
}
function addServices(runtime: any) {
  for(const [id,service,recipe] of [['Z-Image-Turbo','Images','sd-cpp'],['Image-Second','Images','sd-cpp'],['kokoro-v1','Speak','kokoro']])
    runtime.models.push({id,service,recipe,labels:[],loaded:true,downloaded:true,available:true,state:'loaded',policy:'on-demand',runtime:{is_busy:false}});
}
for(const width of [1440,390]) test(`image tests respect model defaults and clear stale results at ${width}px`,async({page,runtime})=>{
  addServices(runtime);const calls:any[]=[];
  await page.route('**/v1/images/generations',route=>{calls.push(route.request().postDataJSON());return route.fulfill({json:{data:[{b64_json:pixel}]}});});
  await page.setViewportSize({width,height:960});await page.goto('/');await navigate(page,'Test services');await navigate(page,'Images');
  await page.getByLabel('Image prompt',{exact:true}).fill('A blue ceramic mug on a wooden desk.');
  await page.getByRole('button',{name:'Generate image',exact:true}).click();await expect(page.getByAltText('Generated image',{exact:true})).toBeVisible();
  expect(calls[0].size).toBeUndefined();
  await page.getByRole('combobox',{name:'Image test size',exact:true}).click();await page.getByRole('option',{name:'1024 × 1024',exact:true}).click();
  await page.getByRole('button',{name:'Generate image',exact:true}).click();await expect.poll(()=>calls.length).toBe(2);expect(calls[1].size).toBe('1024x1024');
  await page.getByLabel('Loaded model',{exact:true}).selectOption('Image-Second');
  await expect(page.getByAltText('Generated image',{exact:true})).toHaveCount(0);
  await expect(page.getByRole('combobox',{name:'Image test size',exact:true})).toContainText('Model default');
  expect(runtime.writes.filter((w:any)=>w.path.includes('/options')||w.path==='juiced/policy')).toEqual([]);
  expect(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth)).toBe(false);
});
test('switching to speech never requests voices from the prior chat model',async({page,runtime})=>{
  addServices(runtime);const requested:string[]=[];
  await page.route('**/v1/audio/voices?*',route=>{requested.push(new URL(route.request().url()).searchParams.get('model')!);return route.fulfill({json:{voices:['af_heart','am_adam']}});});
  await page.goto('/');await navigate(page,'Test services');await navigate(page,'Speak');
  await expect(page.getByRole('combobox',{name:'Speech voice'})).toBeVisible();
  expect(requested).toEqual(['kokoro-v1']);
  await page.getByRole('button',{name:'About Available voices',exact:true}).click();
  await expect(page.getByRole('dialog',{name:'Available voices'})).toContainText('2 English voices');
});
test('protocol selection exposes its actual routes and copies the matching endpoint',async({page,context,runtime})=>{
  await context.grantPermissions(['clipboard-read','clipboard-write']);
  await page.route('**/v1/juiced/protocols',route=>route.fulfill({json:{protocols:[
    {id:'openai',base_path:'/v1',features:['chat.completions'],limits:[]},
    {id:'ollama',base_path:'',features:['chat'],limits:[]},
    {id:'anthropic',base_path:'',features:['messages'],limits:[]},
    {id:'automatic1111',base_path:'',features:['txt2img'],limits:[]},
    {id:'wyoming',features:['describe'],limits:[],listener:{enabled:true,process_running:true,uri:'tcp://0.0.0.0:12345'}},
  ]}}));
  await page.goto('/');await navigate(page,'Service APIs');
  for(const [name,path] of [['OpenAI','/v1/chat/completions'],['Ollama','/api/chat'],['Anthropic','/v1/messages'],['AUTOMATIC1111','/sdapi/v1/txt2img']]) {
    await page.getByRole('button',{name,exact:true}).click();
    await expect(page.getByRole('heading',{name:name+' endpoints',exact:true})).toBeVisible();
    await expect(page.locator('.endpoint-table')).toContainText(path);
    if(name!=='OpenAI')await expect(page.locator('.endpoint-table')).not.toContainText('/v1/chat/completions');
  }
  await page.getByRole('button',{name:'Ollama',exact:true}).click();await page.getByRole('button',{name:'Copy Chat endpoint',exact:true}).click();
  await expect.poll(()=>page.evaluate(()=>navigator.clipboard.readText())).toBe(new URL(page.url()).origin+'/api/chat');
  await page.getByRole('button',{name:'Wyoming',exact:true}).click();
  await expect(page.getByRole('region',{name:'Client connection'})).toContainText('12345');
  await expect(page.locator('.endpoint-table')).toHaveCount(0);await expect(page.locator('.session-header')).toHaveCount(0);expect(runtime.writes).toEqual([]);
});
test('Manage refreshes removal eligibility after policy change and unload',async({page,runtime})=>{
  const model=runtime.models[0];
  await page.route('**/v1/juiced/removal?*',route=>route.fulfill({json:{allowed:!model.loaded&&model.policy!=='hot',description:'Fixture managed download; shared files stay.',reason:model.loaded?'Unload before removing.':'',label:'Delete downloaded weights',action:'delete',model:modelId}}));
  await page.route('**/v1/juiced/operation',route=>{const body=route.request().postDataJSON();runtime.writes.push({path:'juiced/operation',body});model.loaded=false;model.state='unloaded';return route.fulfill({json:{status:'queued'}});});
  await openModel(page);await page.getByRole('button',{name:'Manage',exact:true}).click();
  await expect(page.getByRole('button',{name:'Unload model',exact:true})).toBeDisabled();
  await page.getByRole('combobox',{name:'Loading policy',exact:true}).selectOption('on-demand');
  await expect(page.getByRole('button',{name:'Unload model',exact:true})).toBeEnabled();await page.getByRole('button',{name:'Unload model',exact:true}).click();
  await expect(page.getByRole('button',{name:'Delete downloaded weights…',exact:true})).toBeEnabled();
  await page.getByRole('button',{name:'Delete downloaded weights…',exact:true}).click();
  await expect(page.getByRole('button',{name:'Delete downloaded weights',exact:true})).toBeDisabled();
  await page.getByRole('textbox',{name:'Confirm model removal',exact:true}).fill(modelId);
  await expect(page.getByRole('button',{name:'Delete downloaded weights',exact:true})).toBeEnabled();
  expect(runtime.writes.filter((w:any)=>w.path==='juiced/operation')).toEqual([{path:'juiced/operation',body:{model:modelId,action:'unload'}}]);
});
test('unavailable protocol inventory does not advertise a made-up working connection',async({page,runtime})=>{
  let failed=true;
  await page.route('**/v1/juiced/protocols',route=>failed?route.fulfill({status:503,json:{error:'Fixture protocol inventory is unavailable'}}):route.fallback());
  await page.goto('/');await navigate(page,'Service APIs');
  await expect(page.getByRole('alert')).toContainText('Could not read protocol support');
  await expect(page.getByRole('region',{name:'Client connection'})).toHaveCount(0);
  failed=false;await page.getByRole('button',{name:'Retry support',exact:true}).click();
  await expect(page.getByRole('region',{name:'Client connection'})).toBeVisible();
});
for(const width of [1440,390]) test(`cache drafts validate across tabs and survive navigation at ${width}px`,async({page,runtime})=>{
  await page.setViewportSize({width,height:960});await page.goto('/');await navigate(page,'Cache');
  await page.getByRole('button',{name:'Storage policy',exact:true}).click();await page.getByLabel('Expiry (days)',{exact:true}).fill('0');
  await page.getByRole('button',{name:/^Model overrides/}).click();await page.getByRole('button',{name:'Save changes',exact:true}).click();
  await expect(page.getByRole('alert')).toContainText('Expiry must be');await expect(page.getByLabel('Expiry (days)',{exact:true})).toBeFocused();expect(runtime.writes).toEqual([]);
  await page.getByLabel('Expiry (days)',{exact:true}).fill('14');await navigate(page,'Models');
  const dialog=page.getByRole('dialog',{name:'Discard unsaved cache settings?',exact:true});await expect(dialog).toBeVisible();
  await page.getByRole('button',{name:'Keep editing',exact:true}).click();await expect(page.getByLabel('Expiry (days)',{exact:true})).toHaveValue('14');
  if(width===390)await expect(page.getByLabel('Toggle navigation')).toHaveAttribute('aria-expanded','false');
  await navigate(page,'Models');await page.getByRole('button',{name:'Discard changes',exact:true}).click();await expect(page.getByRole('heading',{name:'Runtime dashboard',exact:true})).toBeVisible();
  await navigate(page,'Cache');await page.getByRole('button',{name:'Storage policy',exact:true}).click();await expect(page.getByLabel('Expiry (days)',{exact:true})).toHaveValue('7');expect(runtime.writes).toEqual([]);
  await page.getByLabel('Disk budget (GiB)',{exact:true}).fill('8.5');await page.getByRole('button',{name:'Save changes',exact:true}).click();await expect.poll(()=>runtime.cache.settings.disk_budget_gib).toBe(8.5);
  expect(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth)).toBe(false);
});
test('clearing an advanced override shows inheritance and sends a reset',async({page,runtime})=>{
  runtime.defaults.downsize_idle_timeout=60;runtime.saved.downsize_idle_timeout=120;
  await openModel(page);await page.getByRole('button',{name:'Advanced',exact:true}).click();
  const input=page.getByRole('spinbutton',{name:'Downsize after idle seconds',exact:true});await expect(input).toHaveValue('120');await input.fill('');
  await expect(input).toHaveValue('');await expect(input).toHaveAttribute('placeholder','60');
  await page.getByRole('button',{name:'Save for next load',exact:true}).click();
  await expect.poll(()=>runtime.writes.find((w:any)=>w.path.endsWith('/options'))?.body.downsize_idle_timeout).toBeNull();
});
