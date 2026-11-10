#!/usr/bin/env python3
"""Model-free contract tests against the actual compiled manager."""
import struct,json,os,pathlib,socket,subprocess,sys,tempfile,time,unittest,urllib.request,urllib.error
from unittest import mock
from admission_fixture import configure_admission
BINARY=str(pathlib.Path(sys.argv.pop(1)).resolve())
class ApiContract(unittest.TestCase):
 @classmethod
 def setUpClass(cls):
  cls.admission_transitions=[]
  cls.temp=tempfile.TemporaryDirectory(prefix='juiced-test-');cls.root=pathlib.Path(cls.temp.name);(cls.root/'config').mkdir();(cls.root/'cache').mkdir();(cls.root/'run').mkdir()
  with socket.socket() as s:s.bind(('127.0.0.1',0));cls.port=s.getsockname()[1]
  cls.probe_dir=cls.root/'fake-cuda';cls.probe_dir.mkdir()
  probe=cls.probe_dir/'llama-server';probe.write_text('#!/bin/sh\ncase "$LD_LIBRARY_PATH" in "'+str(cls.probe_dir)+'"*) printf "CUDA0: Fixture GPU\\n";; *) exit 23;; esac\n');probe.chmod(0o755)
  (cls.root/'config/config.json').write_text(json.dumps({'port':cls.port,'llamacpp':{'cuda_bin':str(cls.probe_dir)},'broadcast':False,'disable_model_filtering':True,'auto_check_model_updates':False}))
  cls.external=cls.root/'external.bin';cls.external.write_bytes(b'preserve this external artifact')
  cls.shared_upload=cls.root/'config/juiced/artifacts/shared/shared.gguf';cls.shared_upload.parent.mkdir(parents=True);cls.shared_upload.write_bytes(b'shared fixture')
  cls.repo=cls.root/'hf/models--Fixture--Removal';(cls.repo/'snapshots/rev').mkdir(parents=True);(cls.repo/'refs').mkdir();(cls.repo/'refs/main').write_text('rev');(cls.repo/'snapshots/rev/one.gguf').write_bytes(b'fixture one');(cls.repo/'snapshots/rev/two.gguf').write_bytes(b'fixture two')
  (cls.root/'config/user_models.json').write_text(json.dumps({'ZZSharedUpload1':{'recipe':'whispercpp','source':'local_path','checkpoints':{'main':str(cls.shared_upload)}},'ZZSharedUpload2':{'recipe':'whispercpp','source':'local_path','checkpoints':{'main':str(cls.shared_upload)}},'ZZUnsupported':{'recipe':'hybrid','juiced_existing_artifact':True,'checkpoints':{'main':str(cls.external)},'labels':['chat']},'ZZRemovalHFOne':{'recipe':'whispercpp','source':'huggingface','checkpoints':{'main':'Fixture/Removal:one.gguf'},'labels':['transcription']},'ZZRemovalHFTwo':{'recipe':'whispercpp','source':'huggingface','checkpoints':{'main':'Fixture/Removal:two.gguf'},'labels':['transcription']},'ZZRemovalExternal':{'recipe':'whispercpp','source':'local_path','checkpoints':{'main':str(cls.external)},'labels':['transcription']},'ZZExternalFile':{'recipe':'whispercpp','source':'local_path','checkpoints':{'main':str(cls.external)},'labels':['transcription']}}))
  def string(value):
   data=value.encode();return struct.pack('<Q',len(data))+data
  cls.execution_file=cls.root/'Llama-Execution-F16.gguf'
  header=b'GGUF'+struct.pack('<IQQ',3,1,2)+string('general.architecture')+struct.pack('<I',8)+string('llama')+string('general.file_type')+struct.pack('<II',4,1)
  header+=string('blk.0.attn_q.weight')+struct.pack('<IQQIQ',2,32,32,1,0)
  cls.execution_file.write_bytes(header+b'\0'*((-len(header))%32)+b'\0'*2048)
  definitions=json.loads((cls.root/'config/user_models.json').read_text());definitions['ZZExecution-F16']={'recipe':'llamacpp','source':'local_path','checkpoints':{'main':str(cls.execution_file)},'labels':['chat']};definitions['ZZProfile-Clear-F16']=dict(definitions['ZZExecution-F16']);(cls.root/'config/user_models.json').write_text(json.dumps(definitions))
  cls.hf_fixture=cls.root/'hf-text-fixture';cls.hf_fixture.mkdir();(cls.hf_fixture/'config.json').write_text(json.dumps({'model_type':'qwen3_5','text_config':{'model_type':'qwen3_5'},'vision_config':{}}));(cls.hf_fixture/'model.safetensors').write_bytes(b'planning fixture only')
  definitions=json.loads((cls.root/'config/user_models.json').read_text());definitions['Qwen3.5-Fixture-BF16']={'recipe':'vllm','source':'local_path','checkpoints':{'main':str(cls.hf_fixture)},'labels':['chat','vision']};(cls.root/'config/user_models.json').write_text(json.dumps(definitions))
  definitions=json.loads((cls.root/'config/user_models.json').read_text());definitions['ZZImage-Variant']={'recipe':'sd-cpp','source':'local_path','checkpoints':{'main':str(cls.execution_file)},'labels':['image'],'recipe_options':{'steps':7}};(cls.root/'config/user_models.json').write_text(json.dumps(definitions))
  cls.long_models=['ZZLong-'+('A'*300), 'ZZOwner/'+('B'*300)];definitions=json.loads((cls.root/'config/user_models.json').read_text())
  for name in cls.long_models:definitions[name]=dict(definitions['ZZExecution-F16'])
  (cls.root/'config/user_models.json').write_text(json.dumps(definitions))
  cls.legacy_jobs=cls.root/'config/jobs.json';cls.legacy_jobs.write_text(json.dumps({'jobs':[{'id':'legacy-fixture','name':'preserved graph','status':'queued','steps':[{'id':'first','op':'invalid-fixture-operation','when':'true','branch':[{'when':'true','goto':'first'}]}]}]}));cls.legacy_jobs_bytes=cls.legacy_jobs.read_bytes()
  cls.log=open(cls.root/'server.log','w');cls.process=subprocess.Popen([BINARY,str(cls.root/'cache'),str(cls.root/'config'),'--host','127.0.0.1','--port',str(cls.port)],stdout=cls.log,stderr=cls.log,env={**os.environ,'HOME':str(cls.root),'XDG_RUNTIME_DIR':str(cls.root/'run'),'HF_HUB_CACHE':str(cls.root/'hf')})
  for _ in range(150):
   try:
    status,x=cls.request('juiced/catalog')
    if status==200 and x['data']:cls.model=x['data'][0]['id'];return
   except (OSError,ValueError):pass
   if cls.process.poll() is not None:raise RuntimeError((cls.root/'server.log').read_text())
   time.sleep(.1)
  raise RuntimeError('Catalog did not initialize')
 @classmethod
 def tearDownClass(cls):
  cls.process.terminate()
  try:cls.process.wait(timeout=20)
  except subprocess.TimeoutExpired:cls.process.kill();cls.process.wait()
  print('Admission transitions:',json.dumps(cls.admission_transitions),flush=True)
  cls.log.close();cls.temp.cleanup()
 @classmethod
 def exchange(cls,path,body=None,raw=False,headers=None,method=None,timeout=15):
  data=body if raw else None if body is None else json.dumps(body).encode()
  q=urllib.request.Request(f'http://127.0.0.1:{cls.port}'+(path if path.startswith('/') else '/v1/'+path),data=data,headers={'Content-Type':'application/octet-stream' if raw else 'application/json',**(headers or {})},method=method)
  try:
   with urllib.request.urlopen(q,timeout=timeout) as r:return r.status,dict(r.headers),json.load(r)
  except urllib.error.HTTPError as r:
   with r:return r.code,dict(r.headers),json.load(r)
 @classmethod
 def request(cls,path,body=None,raw=False,headers=None,method=None):
  status,_,data=cls.exchange(path,body,raw,headers,method)
  return status,data
 def configure_admission(self,settings):
  return configure_admission(self.exchange,settings,self.admission_transitions)
 def test_admission_transition_retry_bounds(self):
  clock=[0.0];calls=[];observations=[]
  busy={'error':{'code':'admission_configuration_busy'}}
  def send(path,body,timeout):
   calls.append((path,body,timeout))
   return (409,{'Retry-After':'0.25'},busy) if len(calls)==1 else (200,{}, {'saved':True})
  with mock.patch('admission_fixture.time.monotonic',side_effect=lambda:clock[0]),mock.patch('admission_fixture.time.sleep',side_effect=lambda seconds:clock.__setitem__(0,clock[0]+seconds)):
   self.assertEqual(configure_admission(send,{'mode':'protected'},observations),{'saved':True})
   self.assertEqual(len(calls),2);self.assertEqual(observations,[{'attempts':2,'busy_rejections':1,'elapsed_seconds':0.25}])
   self.assertEqual(calls[1][2],9.75)
   for status,data in [(409,{'error':{'code':'other'}}),(400,busy),(503,busy),(409,{'error':'invalid'}),(409,None)]:
    rejected=mock.Mock(return_value=(status,{},data))
    with self.assertRaises(AssertionError):configure_admission(rejected,{},[])
    self.assertEqual(rejected.call_count,1)
   forever=mock.Mock(return_value=(409,{'Retry-After':'4'},busy));clock[0]=0.0
   with self.assertRaises(AssertionError):configure_admission(forever,{},[])
   self.assertEqual(clock[0],10);self.assertEqual(forever.call_count,3)
 def test_long_model_ids_keep_native_routes(self):
  for prefix in ['/v1/','/v0/','/api/v1/','/api/v0/']:
   for model in self.long_models:
    path=prefix+'models/'+urllib.parse.quote(model,safe='')
    self.assertEqual(self.request(path)[0],200)
    self.assertEqual(self.request(path+'/files')[0],200)
    self.assertEqual(self.request(path+'/options')[0],200)
    status,result=self.request(path+'/options',{'ctx_size':321});self.assertEqual(status,200,result);self.assertEqual(result['saved']['ctx_size'],321)
    status,result=self.request(path+'/options',method='DELETE');self.assertEqual(status,200,result);self.assertNotIn('ctx_size',result['saved'])
 def test_execution_profiles_and_preparation_contract(self):
  model='ZZExecution-F16'
  for prefix in ['/v1/','/v0/','/api/v1/','/api/v0/']:
   status,plan=self.request(prefix+'juiced/execution?model='+model);self.assertEqual(status,200)
   targets={t['id']:t for t in plan['profiles']};self.assertTrue(targets['llamacpp:cpu']['compatible']);self.assertTrue(targets['llamacpp:cpu']['reuse'])
   self.assertIn('llamacpp:hybrid-rocm',targets);self.assertTrue(targets['llamacpp:hybrid-rocm']['changes'])
   self.assertTrue(any(t['recipe']=='vllm' for t in plan['profiles']))
   status,prepare=self.request(prefix+'juiced/preparation?model='+model);self.assertEqual(status,410);self.assertEqual(prepare['error']['code'],'feature_retired')
   for action in ['', 'plan', 'start', 'resume', 'convert', 'quantize']:
    status,error=self.request(prefix+'juiced/preparation',{'action':action,'model':model,'quant':'Q4_K_M','acknowledge':True});self.assertEqual(status,410,error);self.assertEqual(error['error']['code'],'feature_retired')
   status,legacy=self.request(prefix+'juiced/preparation');self.assertEqual(status,200);self.assertTrue(legacy['retired']);self.assertEqual(legacy['jobs'],{})
  self.assertEqual(self.request('juiced/execution',{'model':model,'profile':'missing'})[0],400)
  self.assertEqual(self.request('juiced/preparation',{'model':model,'quant':'Q4_K_M','acknowledge':False})[0],410)
  self.assertEqual(self.request('juiced/preparation',{'action':'cancel','id':'../../outside'})[0],400)
  status,_=self.request('juiced/execution',{'model':model,'profile':'llamacpp:cpu'});self.assertEqual(status,202)
  for _ in range(100):
   _,plan=self.request('juiced/execution?model='+model)
   if plan['selected']=='llamacpp:cpu':break
   time.sleep(.05)
  self.assertEqual(plan['selected'],'llamacpp:cpu')
  time.sleep(.1)
  _,catalog=self.request('juiced/catalog');entry=next(m for m in catalog['data'] if m['id']==model);self.assertNotEqual(entry.get('operation',{}).get('state'),'failed')
  _,options=self.request('models/'+model+'/options');self.assertEqual(options['effective']['llamacpp_backend'],'cpu')
  self.assertEqual(self.request('juiced/execution',{'model':model,'profile':'registered','expected_selected':'stale'})[0],202)
  time.sleep(.15)
  _,catalog=self.request('juiced/catalog');entry=next(m for m in catalog['data'] if m['id']==model);self.assertEqual(entry['policy'],'disabled');self.assertFalse(entry['loaded'])
  self.assertEqual(self.execution_file.read_bytes()[:4],b'GGUF')
  status,_=self.request('juiced/execution',{'model':model,'profile':'registered','expected_selected':'llamacpp:cpu'});self.assertEqual(status,202)
  for _ in range(100):
   _,plan=self.request('juiced/execution?model='+model)
   if plan['selected']=='registered':break
   time.sleep(.05)
  self.assertEqual(plan['selected'],'registered')
 def test_execution_profile_override_can_be_cleared(self):
  model='ZZProfile-Clear-F16'
  def select(profile):
   status,result=self.request('juiced/execution',{'model':model,'profile':profile});self.assertEqual(status,202,result)
   for _ in range(100):
    _,catalog=self.request('juiced/catalog');entry=next(m for m in catalog['data'] if m['id']==model)
    if entry.get('execution_profile','registered')==profile and not entry.get('operation',{}).get('pending'):return
    time.sleep(.05)
   self.fail('Profile did not settle')
  select('llamacpp:cpu')
  self.assertEqual(self.request('models/'+model+'/options',{'ctx_size':321})[0],200)
  select('registered');select('llamacpp:cpu')
  _,options=self.request('models/'+model+'/options');self.assertEqual(options['effective']['ctx_size'],321)
  status,options=self.request('models/'+model+'/options',{'ctx_size':None});self.assertEqual(status,200)
  self.assertNotIn('ctx_size',options['saved']);self.assertNotEqual(options['effective']['ctx_size'],321)
 def test_conversion_retirement_includes_hf_and_unknown_models(self):
  for model in ['Qwen3.5-Fixture-BF16','Missing-Fixture']:
   status,error=self.request('juiced/preparation?model='+model);self.assertEqual(status,410);self.assertEqual(error['error']['code'],'feature_retired')
 def test_provider_uses_its_declared_backend_option(self):
  model='ZZImage-Variant';status,plan=self.request('juiced/execution?model='+model);self.assertEqual(status,200)
  self.assertTrue(any(t['id']=='sd-cpp:cpu' for t in plan['profiles']))
  status,result=self.request('juiced/execution',{'model':model,'profile':'sd-cpp:cpu'});self.assertEqual(status,202,result)
  for _ in range(100):
   _,catalog=self.request('juiced/catalog');entry=next(m for m in catalog['data'] if m['id']==model)
   if entry.get('execution_profile')=='sd-cpp:cpu' and not entry.get('operation',{}).get('pending'):break
   time.sleep(.05)
  self.assertNotEqual(entry.get('operation',{}).get('state'),'failed')
  _,options=self.request('models/'+model+'/options');self.assertEqual(options['effective']['sd-cpp_backend'],'cpu');self.assertNotIn('sdcpp_backend',options['saved']);self.assertEqual(options['effective']['steps'],7)
 def test_interrupted_execution_trial_recovers_before_startup(self):
  with tempfile.TemporaryDirectory(prefix='juiced-profile-recovery-') as temporary:
   root=pathlib.Path(temporary);(root/'config/juiced').mkdir(parents=True);(root/'cache').mkdir();(root/'run').mkdir()
   with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
   model='Llama-Recovery-F16';previous={'id':'registered','recipe':'llamacpp','options':{'ctx_size':128,'llamacpp_backend':'cpu'}}
   (root/'config/config.json').write_text(json.dumps({'disable_model_filtering':True,'broadcast':False,'no_fetch_executables':True,'auto_check_model_updates':False}))
   (root/'config/user_models.json').write_text(json.dumps({model:{'recipe':'llamacpp','source':'local_path','checkpoints':{'main':str(self.execution_file)},'labels':['chat']}}))
   (root/'config/execution_profiles.json').write_text(json.dumps({'user.'+model:{'selected':'llamacpp:cpu','profiles':{'registered':previous,'llamacpp:cpu':{'id':'llamacpp:cpu','recipe':'llamacpp','options':{'ctx_size':256,'llamacpp_backend':'cpu'}}}}}))
   (root/'config/recipe_options.json').write_text(json.dumps({'user.'+model:{'ctx_size':256,'llamacpp_backend':'cpu'}}))
   (root/'config/juiced/state.json').write_text(json.dumps({'version':1,'revision':0,'policies':{},'imports':{},'cache':{},'execution_trials':{model:{'previous':previous,'was_loaded':False}}}))
   with (root/'server.log').open('w') as log:
    process=subprocess.Popen([BINARY,str(root/'cache'),str(root/'config'),'--host','127.0.0.1','--port',str(port)],stdout=log,stderr=log,env={**os.environ,'HOME':str(root),'XDG_RUNTIME_DIR':str(root/'run')})
    try:
     for _ in range(150):
      try:
       with urllib.request.urlopen(f'http://127.0.0.1:{port}/v1/juiced/execution?model='+model,timeout=1) as response:plan=json.load(response)
       break
      except OSError:time.sleep(.1)
     else:raise AssertionError((root/'server.log').read_text())
     self.assertEqual(plan['selected'],'registered')
     with urllib.request.urlopen(f'http://127.0.0.1:{port}/v1/models/'+model+'/options') as response:options=json.load(response)
     self.assertEqual(options['saved']['ctx_size'],128)
     self.assertEqual(json.loads((root/'config/juiced/state.json').read_text())['execution_trials'],{})
    finally:
     process.terminate();process.wait(timeout=20)
 def test_device_probe_uses_launch_library_environment(self):
  for prefix in ['/v1/','/v0/','/api/v1/','/api/v0/']:
   status,data=self.request(prefix+'juiced/devices?recipe=llamacpp&backend=cuda')
   self.assertEqual(status,200);self.assertEqual(data['devices'],[{'id':'CUDA0','name':'Fixture GPU'}])
 def test_request_metadata_is_bounded_and_private(self):
  marker='REQUEST-CONTENT-MUST-NOT-BE-RETAINED'
  for index in range(205):
   self.request('chat/completions?private='+marker,{'model':'Unregistered-'+marker,'messages':[{'role':'user','content':marker}], 'stream':False},headers={'X-Client-Session-Id':marker})
  for _ in range(30):
   status,history=self.request('juiced/requests')
   if len(history['data'])==200:break
   time.sleep(.05)
  self.assertEqual(status,200);self.assertEqual(len(history['data']),200)
  self.assertFalse(history['content_captured']);self.assertNotIn(marker,json.dumps(history))
  self.assertTrue(all(r['model'] is None and r['status']>=400 and r['duration_ms']>=0 for r in history['data']))
  self.assertTrue(all(set(r)=={'id','timestamp_ms','endpoint','protocol','model','stream','status','duration_ms','input_tokens','output_tokens'} for r in history['data']))
  ids=[r['id'] for r in history['data']];self.assertEqual(ids,sorted(ids,reverse=True))
  for prefix in ['/api/v0/','/api/v1/','/v0/','/v1/']:
   status,result=self.request(prefix+'juiced/requests');self.assertEqual(status,200);self.assertEqual(result['capacity'],200)
 def test_catalog(self):
  s,x=self.request('juiced/catalog');self.assertEqual(s,200);self.assertTrue(x['data']);self.assertTrue(all(m['policy']=='disabled' for m in x['data']));self.assertTrue(all('state'in m and'service'in m for m in x['data']))
 def test_policy_validation_and_revision(self):
  s,x=self.request('juiced/policy',{'model':self.model,'policy':'invalid'});self.assertEqual(s,400)
  _,c=self.request('juiced/catalog');s,_=self.request('juiced/policy',{'model':self.model,'policy':'disabled','revision':c['revision']});self.assertEqual(s,200)
  s,_=self.request('juiced/policy',{'model':self.model,'policy':'hot','revision':c['revision']});self.assertEqual(s,409)
  self.request('juiced/policy',{'model':self.model,'policy':'on-demand'})
 def test_disabled_inference_and_pin_policy(self):
  self.request('juiced/policy',{'model':self.model,'policy':'disabled'})
  for endpoint in ['chat/completions','responses','completions','/api/chat','/api/generate','/v1/messages']:
   s,error=self.request(endpoint,{'model':self.model,'messages':[]});self.assertEqual(s,409,(endpoint,self.model,error))
  s,_=self.request('/internal/pin',{'model':self.model,'pinned':True});self.assertEqual(s,200)
  _,catalog=self.request('juiced/catalog');m=next(x for x in catalog['data'] if x['id']==self.model);self.assertEqual(m['policy'],'hot')
  self.request('/internal/pin',{'model':self.model,'pinned':False})
  _,catalog=self.request('juiced/catalog');m=next(x for x in catalog['data'] if x['id']==self.model);self.assertEqual(m['policy'],'on-demand')
 def test_hot_unload_protection(self):
  self.request('juiced/policy',{'model':self.model,'policy':'hot'});s,_=self.request('juiced/operation',{'model':self.model,'action':'unload'});self.assertEqual(s,409);self.request('juiced/policy',{'model':self.model,'policy':'on-demand'})
 def test_reload_requires_enabled_loaded_model(self):
  self.request('juiced/policy',{'model':self.model,'policy':'disabled'})
  s,_=self.request('juiced/operation',{'model':self.model,'action':'reload'});self.assertEqual(s,409)
  self.request('juiced/policy',{'model':self.model,'policy':'hot'})
  s,_=self.request('juiced/operation',{'model':self.model,'action':'reload'});self.assertEqual(s,409)
  _,catalog=self.request('juiced/catalog');self.assertEqual(next(m['policy'] for m in catalog['data'] if m['id']==self.model),'hot')
  self.request('juiced/policy',{'model':self.model,'policy':'on-demand'})
 def test_device_enumeration_validation(self):
  for prefix in ['/api/v0/','/api/v1/','/v0/','/v1/']:
   s,d=self.request(prefix+'juiced/devices?recipe=llamacpp&backend=cpu');self.assertEqual(s,200);self.assertEqual(d['devices'],[{'id':'none','name':'CPU'}])
   s,_=self.request(prefix+'juiced/devices?recipe=llamacpp&backend=invalid');self.assertEqual(s,400)
   s,_=self.request(prefix+'juiced/devices?recipe=vllm&backend=rocm');self.assertEqual(s,501)
 def test_conversation_storage_is_retired(self):
  for prefix in ['/api/v0/','/api/v1/','/v0/','/v1/']:
   for endpoint in ['juiced/threads','juiced/thread?id=test-thread']:
    status,_=self.request(prefix+endpoint);self.assertEqual(status,410)
   status,_=self.request(prefix+'juiced/thread',{'id':'test-thread','messages':[{'role':'user','content':'do not persist'}]});self.assertEqual(status,410)
  self.assertFalse((self.root/'config/juiced/threads').exists())
 def test_voice_discovery_validation(self):
  for prefix in ['/api/v0/','/api/v1/','/v0/','/v1/']:
   status,_=self.request(prefix+'audio/voices');self.assertEqual(status,400)
   status,_=self.request(prefix+'audio/voices?model='+self.model);self.assertEqual(status,501)
   status,_=self.request(prefix+'audio/voices?model=kokoro-v1');self.assertEqual(status,409)
 def test_disabled_model_discovery(self):
  model='ZZExternalFile'
  self.request('juiced/policy',{'model':model,'policy':'disabled'})
  _,j=self.request('models');self.assertNotIn(model,[m['id'] for m in j['data']])
  _,tags=self.request('/api/tags');self.assertFalse(any(model in m['name'] for m in tags['models']))
  self.request('juiced/policy',{'model':model,'policy':'on-demand'})
  _,tags=self.request('/api/tags');self.assertFalse(any(model in m['name'] for m in tags['models']))
  _,j=self.request('models');self.assertIn(model,[m['id'] for m in j['data']])
  self.request('juiced/policy',{'model':model,'policy':'disabled'})
 def test_protocol_authentication(self):
  with tempfile.TemporaryDirectory(prefix='juiced-auth-') as tmp:
   root=pathlib.Path(tmp);(root/'config').mkdir();(root/'cache').mkdir();(root/'run').mkdir()
   with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
   (root/'config/config.json').write_text(json.dumps({'port':port,'broadcast':False,'auto_check_model_updates':False}))
   (root/'config/jobs.json').write_text(json.dumps({'version':1,'jobs':[{'id':'history-'+str(i),'status':'completed'} for i in range(4)]+[{'id':'pending-cleanup','deleted':True,'status':'interrupted'}]}))
   with open(root/'server.log','w') as log:
    process=subprocess.Popen([BINARY,str(root/'cache'),str(root/'config'),'--host','127.0.0.1','--port',str(port)],stdout=log,stderr=log,env={**os.environ,'HOME':str(root),'XDG_RUNTIME_DIR':str(root/'run'),'LEMONADE_API_KEY':'fixture-regular','LEMONADE_ADMIN_API_KEY':'fixture-admin'})
    def request(path,headers=None,body=None,method=None):
     q=urllib.request.Request(f'http://127.0.0.1:{port}'+path,headers=headers or {},data=None if body is None else json.dumps(body).encode(),method=method)
     try:
      with urllib.request.urlopen(q,timeout=10) as response:return response.status
     except urllib.error.HTTPError as response:return response.code
    try:
     for _ in range(100):
      try:
       if request('/v1/models')==401:break
      except OSError:pass
      time.sleep(.1)
     else:self.fail('Authenticated fixture did not start')
     for path in ['/v1/juiced/protocols','/v1/juiced/capabilities','/sdapi/v1/sd-models','/api/tags']:
      self.assertEqual(request(path),401,path)
      self.assertEqual(request(path,{'Authorization':'Bearer wrong'}),401,path)
      self.assertEqual(request(path,{'Authorization':'Bearer fixture-regular'}),200,path)
     device_path='/v1/juiced/devices?recipe=llamacpp&backend=cpu'
     self.assertEqual(request(device_path),401)
     self.assertEqual(request(device_path,{'Authorization':'Bearer fixture-regular'}),401)
     self.assertEqual(request(device_path,{'Authorization':'Bearer fixture-admin'}),200)
     self.assertEqual(request('/v1/models',{'x-api-key':'fixture-regular','anthropic-version':'2023-06-01'}),200)
     self.assertEqual(request('/v1/juiced/capabilities',{'x-api-key':'fixture-regular'}),200)
     self.assertEqual(request('/v1/models',{'Authorization':'Basic fixture-regular'}),401)
     self.assertEqual(request('/v1/jobs',{'Authorization':'Bearer fixture-regular'}),401)
     self.assertEqual(request('/v1/jobs',{'Authorization':'Bearer fixture-admin'}),200)
     for index,prefix in enumerate(['/api/v0/','/api/v1/','/v0/','/v1/']):
      path=prefix+'jobs/history-'+str(index)
      self.assertEqual(request(path,{'Authorization':'Bearer fixture-regular'},method='DELETE'),401)
      self.assertEqual(request(path,{'Authorization':'Bearer fixture-admin'}),200)
      self.assertEqual(request(path,{'Authorization':'Bearer fixture-admin'},method='DELETE'),200)
      self.assertEqual(request(path,{'Authorization':'Bearer fixture-admin'}),404)
      self.assertEqual(request(prefix+'jobs/pending-cleanup',{'Authorization':'Bearer fixture-admin'},method='DELETE'),409)
     self.assertEqual(request('/v1/install',{'Authorization':'Bearer fixture-regular'},{}),401)
     self.assertEqual(request('/v1/models/Whisper-Tiny/options',{'Authorization':'Bearer fixture-regular'}),200)
     self.assertEqual(request('/v1/models/Whisper-Tiny/options',{'Authorization':'Bearer fixture-regular'},{}),401)
     for prefix in ['/api/v0/','/api/v1/','/v0/','/v1/']:
      self.assertEqual(request(prefix+'slots',{'Authorization':'Bearer fixture-regular'}),401)
      self.assertEqual(request(prefix+'slots/0?action=restore',{'Authorization':'Bearer fixture-regular'},{'filename':'fixture.bin'}),401)
      self.assertEqual(request(prefix+'cloud/auth',{'Authorization':'Bearer fixture-regular'},{}),401)
      self.assertEqual(request(prefix+'cloud/auth/fixture-provider',{'Authorization':'Bearer fixture-regular'},method='DELETE'),401)
     self.assertEqual(request('/v1/messages/count_tokens',{'x-api-key':'fixture-regular'},{}),400)
     self.assertEqual(request('/v1/messages/count_tokens',{'x-api-key':'wrong'},{}),401)
    finally:
     process.terminate()
     try:process.wait(timeout=20)
     except subprocess.TimeoutExpired:process.kill();process.wait()
 def test_effective_authentication_configuration(self):
  with tempfile.TemporaryDirectory(prefix='juiced-effective-auth-') as tmp:
   root=pathlib.Path(tmp)
   for directory in ['config','cache','run']:(root/directory).mkdir()
   with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
   (root/'config/config.json').write_text(json.dumps({'port':port,'broadcast':False,'offline':True,'auto_check_model_updates':False}))
   def request(path,headers,body=None):
    q=urllib.request.Request(f'http://127.0.0.1:{port}'+path,headers=headers,data=None if body is None else json.dumps(body).encode())
    try:
     with urllib.request.urlopen(q,timeout=10) as response:return response.status
    except urllib.error.HTTPError as response:
     with response:return response.code
   def stop(process):
    process.terminate()
    try:process.wait(timeout=20)
    except subprocess.TimeoutExpired:process.kill();process.wait()
   previous_epoch=None
   for api,admin in [('fixture-regular',None),('fixture-regular',''),('','fixture-admin'),('','')]:
    env={k:v for k,v in os.environ.items() if k not in ['LEMONADE_API_KEY','LEMONADE_ADMIN_API_KEY']}
    env.update(HOME=str(root),XDG_RUNTIME_DIR=str(root/'run'),LEMONADE_API_KEY=api)
    if admin is not None:env['LEMONADE_ADMIN_API_KEY']=admin
    with open(root/'server.log','w') as log:
     process=subprocess.Popen([BINARY,str(root/'cache'),str(root/'config'),'--host','127.0.0.1','--port',str(port)],stdout=log,stderr=log,env=env)
     try:
      for _ in range(150):
       try:
        if request('/v1/models',{'Authorization':'Bearer '+(api or admin or '')})==200:break
       except OSError:pass
       if process.poll() is not None:self.fail('Authentication fixture exited')
       time.sleep(.1)
      else:self.fail('Authentication fixture did not start')
      effective_admin=admin or api
      candidates=[{}, {'Authorization':'Basic invalid'}, {'Authorization':'Bearer '}, {'Authorization':'Bearer wrong'}, {'Authorization':'Bearer '+(api or effective_admin)}, {'x-api-key':api or effective_admin}]
      for headers in candidates:
       credential=headers.get('x-api-key',headers.get('Authorization','').removeprefix('Bearer '))
       regular_allowed=not api or bool(credential) and credential in [api,effective_admin]
       operator_allowed=not effective_admin or bool(credential) and credential==effective_admin
       for prefix in ['/api/v0/','/api/v1/','/v0/','/v1/']:
        self.assertEqual(request(prefix+'models',headers),200 if regular_allowed else 401,(prefix,api,admin,headers))
        self.assertEqual(request(prefix+'juiced/admission',headers),200 if operator_allowed else 401,(prefix,api,admin,headers))
       self.assertEqual(request('/internal/config',headers),200 if operator_allowed else 401,('internal/config',api,admin,headers))
       for path,body,accepted in [('/api/tags',None,200),('/sdapi/v1/sd-models',None,200),('/v1/messages/count_tokens',{},400),('/mcp',{'jsonrpc':'2.0','id':1,'method':'ping'},200)]:
        self.assertEqual(request(path,headers,body),accepted if regular_allowed else 401,(path,api,admin,headers))
      epoch=json.loads((root/'config/juiced/state.json').read_text())['authentication']['epoch']
      if api and admin=='':self.assertEqual(epoch,previous_epoch,'Equivalent unset/empty override must retain restart namespace')
      previous_epoch=epoch
     finally:stop(process)
   before=(root/'config/juiced/state.json').read_bytes();temporary=root/'config/juiced/state.json.tmp';temporary.mkdir()
   env.update(LEMONADE_API_KEY='fixture-rejected',LEMONADE_ADMIN_API_KEY='')
   with open(root/'rejected.log','w') as log:
    process=subprocess.Popen([BINARY,str(root/'cache'),str(root/'config'),'--host','127.0.0.1','--port',str(port)],stdout=log,stderr=log,env=env)
    try:process.wait(timeout=20);self.assertNotEqual(process.returncode,0);self.assertEqual((root/'config/juiced/state.json').read_bytes(),before)
    finally:stop(process);temporary.rmdir()
 def test_native_admission_controls(self):
  for prefix in ['/v1/','/v0/','/api/v1/','/api/v0/']:
   status,state=self.request(prefix+'juiced/admission');self.assertEqual(status,200);self.assertEqual(state['mode'],'legacy');self.assertFalse(state['latency_qualified'])
   self.assertEqual(state['optional_limits']['schema_version'],1);self.assertIsNone(state['optional_limits']['principal_pending_limit'])
   self.assertEqual(self.request(prefix+'juiced/admission',{'paused':'false'})[0],400)
   status,state=self.request(prefix+'juiced/admission',{'paused':True});self.assertEqual(status,200);self.assertTrue(state['paused'])
   status,state=self.request(prefix+'juiced/admission',{'paused':False});self.assertEqual(status,200);self.assertFalse(state['paused'])
  for config in [{'mode':'unknown'},{'mode':42},{'pending_limit':-1},{'pools':{'host':{'source':42}}}]:
   self.assertEqual(self.request('/internal/set',{'admission':config})[0],400)
  self.assertEqual(self.request('/internal/set',{'admission':{},'port':1234})[0],400)
  self.configure_admission({'mode':'protected'})
  for limit in [3,None]:
   self.configure_admission({'principal_pending_limit':limit})
   for prefix in ['/v1/','/v0/','/api/v1/','/api/v0/']:
    status,state=self.request(prefix+'juiced/admission');self.assertEqual(status,200);self.assertEqual(state['optional_limits']['principal_pending_limit'],limit)
   saved=json.loads((self.root/'config/config.json').read_text());self.assertEqual(saved['admission'],{'principal_pending_limit':limit})
  self.configure_admission({})
  saved=json.loads((self.root/'config/config.json').read_text());self.assertEqual(saved['admission'],{})
 def test_context_metadata_sources(self):
  model='ZZExecution-F16'
  original=self.request('models/'+model+'/options')[1]['saved'].get('ctx_size')
  self.assertEqual(self.request('juiced/policy',{'model':model,'policy':'on-demand'})[0],200)
  try:
   self.assertEqual(self.request('models/'+model+'/options',{'ctx_size':-1})[0],200)
   for prefix in ['/api/v0/','/api/v1/','/v0/','/v1/']:
    status,data=self.request(prefix+'models/'+model);self.assertEqual(status,200,data)
    self.assertTrue(data['context']['configured_automatic']);self.assertIsNone(data['context']['configured_tokens'])
    self.assertIsNone(data['context']['observed_per_request_tokens']);self.assertNotIn('context_length',data)
   status,shown=self.request('/api/show',{'model':model});self.assertEqual(status,200,shown)
   self.assertEqual(shown['parameters'],'');self.assertNotIn('llama.context_length',shown['model_info'])
   self.assertNotIn('general.parameter_count',shown['model_info'])
   self.assertEqual(self.request('models/'+model+'/options',{'ctx_size':1234})[0],200)
   for prefix in ['/api/v0/','/api/v1/','/v0/','/v1/']:
    status,data=self.request(prefix+'models/'+model);self.assertEqual(status,200,data)
    self.assertEqual(data['context_length'],1234);self.assertEqual(data['context']['advertised_source'],'configured_options')
    self.assertIsNone(data['context']['observed_per_request_tokens'])
  finally:
   self.request('models/'+model+'/options',{'ctx_size':original})
   self.request('juiced/policy',{'model':model,'policy':'disabled'})
 def test_resource_memory_snapshot(self):
  for prefix in ['/api/v0/','/api/v1/','/v0/','/v1/']:
   status,stats=self.request(prefix+'stats');self.assertEqual(status,200,stats)
   self.assertEqual(stats['live']['active_scope'],'native-worker-calls')
   for key in ['active_requests','generating_requests']:
    self.assertIsInstance(stats['live'][key],int);self.assertGreaterEqual(stats['live'][key],0)
   status,data=self.request(prefix+'system-stats');self.assertEqual(status,200,data)
   memory=data['memory_budget']
   self.assertEqual(memory['schema_version'],1)
   self.assertEqual(memory['physical_pool'],'host-ram')
   self.assertEqual(memory['reservation_status'],'not-reserved')
   self.assertEqual(memory['charged_memory_covers_device_allocations'],'unverified')
   self.assertIsInstance(memory['visible_constraints_complete'],bool)
   for key in ['host_total_bytes','host_available_bytes','available_bytes']:
    value=memory[key]
    self.assertTrue(value is None or type(value) is int and value>=0)
   if memory['available_bytes'] is not None:
    self.assertLessEqual(memory['available_bytes'],memory['host_available_bytes'])
   for gpu in data.get('gpus',[]):
    if gpu['memory_type']=='shared':self.assertEqual(gpu['physical_pool'],'host-ram')
    if gpu['memory_type']=='unknown':self.assertIsNone(gpu['physical_pool'])
 def test_capability_contract(self):
  model='ZZExecution-F16'
  self.request('juiced/policy',{'model':model,'policy':'on-demand'})
  try:
   for prefix in ['/api/v0/','/api/v1/','/v0/','/v1/']:
    status,data=self.request(prefix+'juiced/capabilities');self.assertEqual(status,200,data)
    self.assertEqual(data['schema_version'],1);self.assertEqual(data['scope']['agent_execution'],False)
    self.assertEqual(data['scope']['user_workflows'],False)
    self.assertEqual(data['identity']['snapshot_namespace'],'principal-v1')
    self.assertFalse(data['identity']['session_ids_are_access_control'])
    entry=next(x for x in data['models'] if x['model']==model)
    self.assertEqual(entry['artifact']['architecture'],'llama')
    self.assertEqual(entry['execution']['performance_qualification'],'not_recorded')
    self.assertIsNone(entry['limits']['concurrent_requests'])
    self.assertEqual(entry['operations'][0]['id'],'chat.completions')
    self.assertIsNone(entry['operations'][0]['streaming'])
   self.request('juiced/policy',{'model':model,'policy':'disabled'})
   _,data=self.request('juiced/capabilities');self.assertNotIn(model,[x['model'] for x in data['models']])
   _,well_known=self.request('/.well-known/juiced.json');self.assertEqual(well_known['capabilities'],'/v1/juiced/capabilities')
  finally:self.request('juiced/policy',{'model':model,'policy':'disabled'})
 def test_runtime_only_jobs(self):
  for prefix in ['/api/v0/','/api/v1/','/v0/','/v1/']:
   for body in [{'steps':[{'op':'model.load','params':{'model':'invalid'}}]}, {'definition':{'steps':[{'op':'model.load'}]}}, {'name':'graph','inputs':{},'definition':{'steps':[]}}]:
    status,_=self.request(prefix+'jobs',body);self.assertEqual(status,501)
   for action in ['resume','pause','interrupt']:
    status,_=self.request(prefix+'jobs/legacy-fixture/'+action,{});self.assertEqual(status,501)
 def test_legacy_jobs_preserved_and_not_resumed(self):
  status,data=self.request('jobs/legacy-fixture');self.assertEqual(status,200,data)
  self.assertEqual(data['status'],'interrupted')
  self.assertEqual(data['steps'][0]['op'],'invalid-fixture-operation')
  status,_=self.request('jobs/legacy-fixture/resume',{});self.assertEqual(status,501)
  _,data=self.request('jobs/legacy-fixture');self.assertEqual(data['status'],'interrupted')
  self.assertEqual(self.legacy_jobs.read_bytes(),self.legacy_jobs_bytes)
 def test_runtime_only_mcp(self):
  status,data=self.request('/mcp',{'jsonrpc':'2.0','id':1,'method':'tools/list'})
  self.assertEqual(status,200,data);self.assertNotIn('lemonade_omni',[x['name'] for x in data['result']['tools']])
  status,data=self.request('/mcp',{'jsonrpc':'2.0','id':2,'method':'tools/call','params':{'name':'lemonade_omni','arguments':{'messages':[{'role':'user','content':'fixture'}]}}})
  self.assertEqual(status,200);self.assertTrue(data['result']['isError'])
 def test_agent_collection_policy(self):
  _,catalog=self.request('juiced/catalog')
  omni=[x for x in catalog['data'] if x['recipe']=='collection.omni']
  self.assertTrue(omni,'Expected native collection fixture catalogue')
  model=omni[0]['id'];self.assertFalse(omni[0]['available'])
  self.assertFalse(omni[0]['capability_contract']['runtime_service'])
  self.assertEqual(omni[0]['capability_contract']['operations'],[])
  status,_=self.request('juiced/policy',{'model':model,'policy':'hot'});self.assertEqual(status,400)
  self.assertEqual(self.request('/internal/pin',{'model':model,'pinned':True})[0],400)
  _,after=self.request('juiced/catalog');self.assertNotEqual(next(m for m in after['data'] if m['id']==model)['policy'],'hot')
  for prefix in ['/api/v0/','/api/v1/','/v0/','/v1/']:
   for stream in [False,True]:
    status,_=self.request(prefix+'chat/completions',{'model':model,'stream':stream,'messages':[{'role':'user','content':'fixture'}]});self.assertEqual(status,409)
  status,_=self.request('chat/completions',{'model':model,'messages':[{'role':'user','content':'x'*270000}]});self.assertEqual(status,409)
  self.assertEqual(self.request('/internal/aliases',{'alias':'runtime-only-omni-alias','target':model})[0],200)
  for name in [model,'runtime-only-omni-alias']:
   self.assertEqual(self.request('chat/completions',{'model':name,'messages':[{'role':'user','content':'fixture'}]})[0],409)
  _,models=self.request('models?show_all=true');self.assertNotIn(model,[m['id'] for m in models['data']]);self.assertNotIn('runtime-only-omni-alias',[m['id'] for m in models['data']])
  self.request('/internal/aliases/runtime-only-omni-alias',method='DELETE')
 def test_protocol_manifest(self):
  for prefix in ['/api/v0/','/api/v1/','/v0/','/v1/']:
   status,data=self.request(prefix+'juiced/protocols');self.assertEqual(status,200)
   protocols={p['id']:p for p in data['protocols']};self.assertEqual(set(protocols),{'openai','ollama','anthropic','automatic1111','wyoming'})
   self.assertFalse(protocols['wyoming']['listener']['enabled'])
 def test_sdapi_policy_and_validation(self):
  status,data=self.request('/sdapi/v1/sd-models');self.assertEqual(status,200);self.assertEqual(data,[])
  for endpoint in ['txt2img','options']:
   status,_=self.request('/sdapi/v1/'+endpoint,{'model':'kokoro-v1','prompt':'A lighthouse'});self.assertIn(status,[400,409])
  status,_=self.request('/sdapi/v1/txt2img',[]);self.assertEqual(status,409)
  for endpoint in ['interrupt','img2img']:
   status,_=self.request('/sdapi/v1/'+endpoint,{});self.assertEqual(status,501)
 def test_ollama_lifecycle_cannot_bypass_manager(self):
  status,_=self.request('/api/delete',{'model':'ZZExternalFile'},method='DELETE');self.assertEqual(status,501);self.assertTrue(self.external.exists())
  status,_=self.request('/api/pull',{'model':'Fixture/ShouldNotDownload'});self.assertEqual(status,501)
  status,_=self.request('/api/show',{'model':'kokoro-v1'});self.assertEqual(status,404)
  self.request('juiced/policy',{'model':self.model,'policy':'hot'})
  for endpoint in ['chat','generate']:
   status,_=self.request('/api/'+endpoint,{'model':self.model,'keep_alive':0});self.assertEqual(status,409)
  self.request('juiced/policy',{'model':self.model,'policy':'disabled'})
 def test_anthropic_count_policy(self):
  status,_=self.request('/v1/messages/count_tokens',{});self.assertEqual(status,400)
  self.request('juiced/policy',{'model':self.model,'policy':'disabled'})
  status,_=self.request('/v1/messages/count_tokens',{'model':self.model,'messages':[]});self.assertEqual(status,409)
 def test_model_projections(self):
  status,data=self.request('models',headers={'anthropic-version':'2023-06-01'});self.assertEqual(status,200);self.assertFalse(data['has_more'])
  self.assertTrue(all(x['type']=='model' and x['display_name']==x['id'] for x in data['data']))
  self.request('juiced/policy',{'model':'ZZRemovalExternal','policy':'on-demand'})
  _,data=self.request('models?service=transcribe');self.assertIn('ZZRemovalExternal',[m['id'] for m in data['data']])
  _,data=self.request('models?service=chat');self.assertNotIn('ZZRemovalExternal',[m['id'] for m in data['data']])
  self.request('juiced/policy',{'model':'ZZRemovalExternal','policy':'disabled'})
 def test_cache_recovery_protocol(self):
  for prefix in ['/api/v0/','/api/v1/','/v0/','/v1/']:
   status,data=self.request(prefix+'juiced/cache/recover',{});self.assertEqual(status,200);self.assertIn('pending_writes',data);self.assertIn('disk',data)
   status,_=self.request(prefix+'juiced/cache/recover',{'filename':'untrusted.bin'});self.assertEqual(status,400)
 def test_cache_snapshot_validation(self):
  for prefix in ['/api/v0/','/api/v1/','/v0/','/v1/']:
   status,_=self.request(prefix+'juiced/cache/retain',{'snapshot':'missing','retained':True});self.assertEqual(status,404)
   status,_=self.request(prefix+'juiced/cache/retain',{'snapshot':'missing','retained':'yes'});self.assertEqual(status,400)
 def test_cache_validation(self):
  s,_=self.request('juiced/cache',{'mode':'persistent'});self.assertEqual(s,422)
  s,_=self.request('juiced/cache',{'retention_days':-1});self.assertEqual(s,400)
  s,x=self.request('juiced/cache',{'mode':'off'});self.assertEqual(s,200);self.assertEqual(x['settings']['mode'],'off')
 def test_model_cache_modes(self):
  for invalid in [[],{'missing-model':'memory'},{self.model:'invalid'},{self.model:False}]:
   s,_=self.request('juiced/cache',{'model_modes':invalid});self.assertEqual(s,400)
  for prefix in ['/api/v0/','/api/v1/','/v0/','/v1/']:
   s,x=self.request(prefix+'juiced/cache',{'model_modes':{self.model:'memory'}});self.assertEqual(s,200);self.assertEqual(x['settings']['model_modes'],{self.model:'memory'})
  persisted=json.loads((self.root/'config/juiced/state.json').read_text());self.assertEqual(persisted['cache']['model_modes'],{self.model:'memory'})
  s,x=self.request('juiced/cache',{'model_modes':{}});self.assertEqual(s,200);self.assertEqual(x['settings']['model_modes'],{})
 def test_chunk_offset_and_invalid_gguf(self):
  s,j=self.request('juiced/import/start',{'filename':'bad.gguf','size':32});self.assertEqual(s,201)
  for offset in ['-1','0suffix','18446744073709551616']:
   s,_=self.request('juiced/import/chunk?id='+j['id']+'&offset='+offset,b'x',True);self.assertEqual(s,400)
  s,_=self.request('juiced/import/chunk?id='+j['id']+'&offset=1',b'x'*32,True);self.assertEqual(s,409)
  s,_=self.request('juiced/import/chunk?id='+j['id']+'&offset=0',b'x'*32,True);self.assertEqual(s,200)
  s,_=self.request('juiced/import/finish',{'id':j['id'],'name':'BadFixture'});self.assertEqual(s,400)
  s,_=self.request('juiced/import/cancel',{'id':j['id']});self.assertEqual(s,200)
 def test_local_path_delete_preserves_file(self):
  before=self.external.read_bytes();s,_=self.request('delete',{'model_name':'user.ZZExternalFile'});self.assertEqual(s,200)
  self.assertEqual(self.external.read_bytes(),before)
  self.assertNotIn('ZZExternalFile',json.loads((self.root/'config/user_models.json').read_text()))
 def test_removal_external_registration(self):
  model='ZZRemovalExternal';before=self.external.read_bytes()
  for prefix in ['/api/v0/','/api/v1/','/v0/','/v1/']:
   status,plan=self.request(prefix+'juiced/removal?model='+model);self.assertEqual(status,200);self.assertTrue(plan['allowed']);self.assertEqual(plan['action'],'unregister')
  status,_=self.request('juiced/removal',{'model':model,'expected':plan,'confirmation':'wrong-name'});self.assertEqual(status,400)
  status,_=self.request('juiced/removal',{'model':model,'expected':{**plan,'action':'upload'},'confirmation':model});self.assertEqual(status,409)
  status,_=self.request('juiced/removal',{'model':model,'expected':plan,'confirmation':model});self.assertEqual(status,200)
  self.assertEqual(self.external.read_bytes(),before)
  self.assertNotIn(model,json.loads((self.root/'config/user_models.json').read_text()))
 def test_removal_managed_variant_preserves_sibling(self):
  model='ZZRemovalHFOne';status,plan=self.request('juiced/removal?model='+model);self.assertEqual(status,200);self.assertEqual(plan['action'],'cache');self.assertTrue(plan['allowed'])
  status,_=self.request('juiced/removal',{'model':model,'expected':plan,'confirmation':model});self.assertEqual(status,200)
  self.assertFalse((self.repo/'snapshots/rev/one.gguf').exists());self.assertEqual((self.repo/'snapshots/rev/two.gguf').read_bytes(),b'fixture two')
 def test_removal_hot_protection(self):
  self.request('juiced/policy',{'model':self.model,'policy':'hot'})
  status,plan=self.request('juiced/removal?model='+self.model);self.assertEqual(status,200);self.assertFalse(plan['allowed'])
  status,_=self.request('juiced/removal',{'model':self.model,'expected':plan,'confirmation':self.model});self.assertEqual(status,409)
  status,_=self.request('delete',{'model_name':self.model});self.assertEqual(status,409)
  self.request('juiced/policy',{'model':self.model,'policy':'disabled'})
 def test_removal_uploaded_weights(self):
  def string(s):
   b=s.encode();return struct.pack('<Q',len(b))+b
  payload=b'GGUF'+struct.pack('<IQQ',3,0,1)+string('general.architecture')+struct.pack('<I',8)+string('llama')
  status,job=self.request('juiced/import/start',{'filename':'RemovalFixture.gguf','size':len(payload)});self.assertEqual(status,201)
  status,_=self.request('juiced/import/chunk?id='+job['id']+'&offset=0',payload,True);self.assertEqual(status,200)
  status,model=self.request('juiced/import/finish',{'id':job['id'],'name':'RemovalFixture','recipe':'llamacpp'});self.assertEqual(status,201,model)
  file=pathlib.Path(model['path']);self.assertTrue(file.exists())
  status,plan=self.request('juiced/removal?model=RemovalFixture');self.assertEqual(status,200);self.assertEqual(plan['action'],'upload');self.assertTrue(plan['allowed'])
  status,_=self.request('juiced/removal',{'model':'RemovalFixture','expected':plan,'confirmation':'RemovalFixture'});self.assertEqual(status,200)
  self.assertFalse(file.exists());self.assertTrue(self.external.exists())
  status,_=self.request('juiced/import/finish',{'id':job['id']});self.assertEqual(status,410)
 def test_removal_shared_upload_preserves_file(self):
  model='ZZSharedUpload1';status,plan=self.request('juiced/removal?model='+model);self.assertEqual(status,200,plan);self.assertEqual(plan['action'],'unregister')
  status,_=self.request('juiced/removal',{'model':model,'expected':plan,'confirmation':model});self.assertEqual(status,200);self.assertEqual(self.shared_upload.read_bytes(),b'shared fixture')
  self.assertIn('ZZSharedUpload2',json.loads((self.root/'config/user_models.json').read_text()))
 def test_removal_unsupported_registration(self):
  model='ZZUnsupported';before=self.external.read_bytes()
  status,plan=self.request('juiced/removal?model='+model);self.assertEqual(status,200,plan);self.assertEqual(plan['action'],'unregister')
  status,_=self.request('juiced/removal',{'model':model,'expected':plan,'confirmation':model});self.assertEqual(status,200);self.assertEqual(self.external.read_bytes(),before)
  self.assertNotIn(model,json.loads((self.root/'config/user_models.json').read_text()))
 def test_upload_path_rejected(self):
  s,_=self.request('juiced/import/start',{'filename':'../escape.gguf','size':32});self.assertEqual(s,400)
if __name__=='__main__':unittest.main(verbosity=2)
