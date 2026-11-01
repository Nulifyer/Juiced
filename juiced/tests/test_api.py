#!/usr/bin/env python3
"""Model-free contract tests against the actual compiled manager."""
import json,os,pathlib,socket,subprocess,sys,tempfile,time,unittest,urllib.request,urllib.error
BINARY=str(pathlib.Path(sys.argv.pop(1)).resolve())
class ApiContract(unittest.TestCase):
 @classmethod
 def setUpClass(cls):
  cls.temp=tempfile.TemporaryDirectory(prefix='juiced-test-');cls.root=pathlib.Path(cls.temp.name);(cls.root/'config').mkdir();(cls.root/'cache').mkdir();(cls.root/'run').mkdir()
  with socket.socket() as s:s.bind(('127.0.0.1',0));cls.port=s.getsockname()[1]
  (cls.root/'config/config.json').write_text(json.dumps({'port':cls.port,'broadcast':False,'disable_model_filtering':True,'auto_check_model_updates':False}))
  cls.log=open(cls.root/'server.log','w');cls.process=subprocess.Popen([BINARY,str(cls.root/'cache'),str(cls.root/'config'),'--host','127.0.0.1','--port',str(cls.port)],stdout=cls.log,stderr=cls.log,env={**os.environ,'HOME':str(cls.root),'XDG_RUNTIME_DIR':str(cls.root/'run')})
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
 def request(cls,path,body=None,raw=False):
  data=body if raw else None if body is None else json.dumps(body).encode()
  q=urllib.request.Request(f'http://127.0.0.1:{cls.port}/v1/'+path,data=data,headers={'Content-Type':'application/octet-stream' if raw else 'application/json'})
  try:
   with urllib.request.urlopen(q,timeout=15) as r:return r.status,json.load(r)
  except urllib.error.HTTPError as r:
   with r:return r.code,json.load(r)
 def test_catalog(self):
  s,x=self.request('juiced/catalog');self.assertEqual(s,200);self.assertTrue(x['data']);self.assertTrue(all(m['policy']=='on-demand' for m in x['data']));self.assertTrue(all('state'in m and'service'in m for m in x['data']))
 def test_policy_validation_and_revision(self):
  s,x=self.request('juiced/policy',{'model':self.model,'policy':'invalid'});self.assertEqual(s,400)
  _,c=self.request('juiced/catalog');s,_=self.request('juiced/policy',{'model':self.model,'policy':'disabled','revision':c['revision']});self.assertEqual(s,200)
  s,_=self.request('juiced/policy',{'model':self.model,'policy':'hot','revision':c['revision']});self.assertEqual(s,409)
  self.request('juiced/policy',{'model':self.model,'policy':'on-demand'})
 def test_hot_unload_protection(self):
  self.request('juiced/policy',{'model':self.model,'policy':'hot'});s,_=self.request('juiced/operation',{'model':self.model,'action':'unload'});self.assertEqual(s,409);self.request('juiced/policy',{'model':self.model,'policy':'on-demand'})
 def test_thread_revision_and_path(self):
  s,t=self.request('juiced/thread',{'id':'test-thread','title':'Retained fixture','messages':[{'role':'user','content':'hello'}]});self.assertEqual(s,200)
  s,updated=self.request('juiced/thread',{**t,'shelf':'settled'});self.assertEqual(s,200);self.assertEqual(updated['shelf'],'settled')
  s,_=self.request('juiced/thread',t);self.assertEqual(s,409)
  s,_=self.request('juiced/thread',{'id':'../escape'});self.assertEqual(s,400)
  _,got=self.request('juiced/thread?id=test-thread');self.assertEqual(got['messages'],t['messages'])
 def test_cache_validation(self):
  s,_=self.request('juiced/cache',{'mode':'persistent'});self.assertEqual(s,422)
  s,_=self.request('juiced/cache',{'retention_days':-1});self.assertEqual(s,400)
  s,x=self.request('juiced/cache',{'mode':'off'});self.assertEqual(s,200);self.assertEqual(x['settings']['mode'],'off')
 def test_chunk_offset_and_invalid_gguf(self):
  s,j=self.request('juiced/import/start',{'filename':'bad.gguf','size':32});self.assertEqual(s,201)
  s,_=self.request('juiced/import/chunk?id='+j['id']+'&offset=1',b'x'*32,True);self.assertEqual(s,409)
  s,_=self.request('juiced/import/chunk?id='+j['id']+'&offset=0',b'x'*32,True);self.assertEqual(s,200)
  s,_=self.request('juiced/import/finish',{'id':j['id'],'name':'BadFixture'});self.assertEqual(s,400)
  s,_=self.request('juiced/import/cancel',{'id':j['id']});self.assertEqual(s,200)
 def test_upload_path_rejected(self):
  s,_=self.request('juiced/import/start',{'filename':'../escape.gguf','size':32});self.assertEqual(s,400)
if __name__=='__main__':unittest.main(verbosity=2)
