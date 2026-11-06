#!/usr/bin/env python3
"""Model-free contract tests against the actual compiled manager."""
import struct,json,os,pathlib,socket,subprocess,sys,tempfile,time,unittest,urllib.request,urllib.error
BINARY=str(pathlib.Path(sys.argv.pop(1)).resolve())
class ApiContract(unittest.TestCase):
 @classmethod
 def setUpClass(cls):
  cls.temp=tempfile.TemporaryDirectory(prefix='juiced-test-');cls.root=pathlib.Path(cls.temp.name);(cls.root/'config').mkdir();(cls.root/'cache').mkdir();(cls.root/'run').mkdir()
  with socket.socket() as s:s.bind(('127.0.0.1',0));cls.port=s.getsockname()[1]
  cls.probe_dir=cls.root/'fake-cuda';cls.probe_dir.mkdir()
  probe=cls.probe_dir/'llama-server';probe.write_text('#!/bin/sh\ncase "$LD_LIBRARY_PATH" in "'+str(cls.probe_dir)+'"*) printf "CUDA0: Fixture GPU\\n";; *) exit 23;; esac\n');probe.chmod(0o755)
  (cls.root/'config/config.json').write_text(json.dumps({'port':cls.port,'llamacpp':{'cuda_bin':str(cls.probe_dir)},'broadcast':False,'disable_model_filtering':True,'auto_check_model_updates':False}))
  cls.external=cls.root/'external.bin';cls.external.write_bytes(b'preserve this external artifact')
  cls.shared_upload=cls.root/'config/juiced/artifacts/shared/shared.gguf';cls.shared_upload.parent.mkdir(parents=True);cls.shared_upload.write_bytes(b'shared fixture')
  cls.repo=cls.root/'hf/models--Fixture--Removal';(cls.repo/'snapshots/rev').mkdir(parents=True);(cls.repo/'refs').mkdir();(cls.repo/'refs/main').write_text('rev');(cls.repo/'snapshots/rev/one.gguf').write_bytes(b'fixture one');(cls.repo/'snapshots/rev/two.gguf').write_bytes(b'fixture two')
  (cls.root/'config/user_models.json').write_text(json.dumps({'ZZSharedUpload1':{'recipe':'whispercpp','source':'local_path','checkpoints':{'main':str(cls.shared_upload)}},'ZZSharedUpload2':{'recipe':'whispercpp','source':'local_path','checkpoints':{'main':str(cls.shared_upload)}},'ZZUnsupported':{'recipe':'hybrid','juiced_existing_artifact':True,'checkpoints':{'main':str(cls.external)},'labels':['chat']},'ZZRemovalHFOne':{'recipe':'whispercpp','source':'huggingface','checkpoints':{'main':'Fixture/Removal:one.gguf'},'labels':['transcription']},'ZZRemovalHFTwo':{'recipe':'whispercpp','source':'huggingface','checkpoints':{'main':'Fixture/Removal:two.gguf'},'labels':['transcription']},'ZZRemovalExternal':{'recipe':'whispercpp','source':'local_path','checkpoints':{'main':str(cls.external)},'labels':['transcription']},'ZZExternalFile':{'recipe':'whispercpp','source':'local_path','checkpoints':{'main':str(cls.external)},'labels':['transcription']}}))
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
  cls.log.close();cls.temp.cleanup()
 @classmethod
 def request(cls,path,body=None,raw=False,headers=None,method=None):
  data=body if raw else None if body is None else json.dumps(body).encode()
  q=urllib.request.Request(f'http://127.0.0.1:{cls.port}'+(path if path.startswith('/') else '/v1/'+path),data=data,headers={'Content-Type':'application/octet-stream' if raw else 'application/json',**(headers or {})},method=method)
  try:
   with urllib.request.urlopen(q,timeout=15) as r:return r.status,json.load(r)
  except urllib.error.HTTPError as r:
   with r:return r.code,json.load(r)
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
   with open(root/'server.log','w') as log:
    process=subprocess.Popen([BINARY,str(root/'cache'),str(root/'config'),'--host','127.0.0.1','--port',str(port)],stdout=log,stderr=log,env={**os.environ,'HOME':str(root),'XDG_RUNTIME_DIR':str(root/'run'),'LEMONADE_API_KEY':'fixture-regular','LEMONADE_ADMIN_API_KEY':'fixture-admin'})
    def request(path,headers=None,body=None):
     q=urllib.request.Request(f'http://127.0.0.1:{port}'+path,headers=headers or {},data=None if body is None else json.dumps(body).encode())
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
     for path in ['/v1/juiced/protocols','/sdapi/v1/sd-models','/api/tags']:
      self.assertEqual(request(path),401,path)
      self.assertEqual(request(path,{'Authorization':'Bearer wrong'}),401,path)
      self.assertEqual(request(path,{'Authorization':'Bearer fixture-regular'}),200,path)
     device_path='/v1/juiced/devices?recipe=llamacpp&backend=cpu'
     self.assertEqual(request(device_path),401)
     self.assertEqual(request(device_path,{'Authorization':'Bearer fixture-regular'}),401)
     self.assertEqual(request(device_path,{'Authorization':'Bearer fixture-admin'}),200)
     self.assertEqual(request('/v1/models',{'x-api-key':'fixture-regular','anthropic-version':'2023-06-01'}),200)
     self.assertEqual(request('/v1/messages/count_tokens',{'x-api-key':'fixture-regular'},{}),400)
     self.assertEqual(request('/v1/messages/count_tokens',{'x-api-key':'wrong'},{}),401)
    finally:
     process.terminate()
     try:process.wait(timeout=20)
     except subprocess.TimeoutExpired:process.kill();process.wait()
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
