#!/usr/bin/env python3
"""Real engine regression gate: useful restore, SSE/JSON snapshots and forget.

Uses ggml-org's 19 MB Stories15M Q4_0 fixture. This checks serving/state behavior,
not model quality. The model file must already be present in the server namespace.
"""
import argparse,json,shlex,subprocess,time,urllib.request,urllib.error
from pathlib import Path
p=argparse.ArgumentParser();p.add_argument('--url',required=True);p.add_argument('--model-file',required=True);p.add_argument('--snapshots',required=True);p.add_argument('--container');p.add_argument('--template-file');p.add_argument('--environment-template',action='store_true');p.add_argument('--out',required=True);a=p.parse_args();base=a.url.rstrip('/')+'/v1/';model='Stories15M-Q4_0'
def call(path,body=None):
 q=urllib.request.Request(base+path,data=None if body is None else json.dumps(body).encode(),headers={'Content-Type':'application/json'})
 try:
  with urllib.request.urlopen(q,timeout=180) as r:return json.load(r)
 except urllib.error.HTTPError as e:
  with e:raise RuntimeError(e.read().decode()) from e
def engine(address,path,body=None):
 if not a.container:
  q=urllib.request.Request(address+path,data=None if body is None else json.dumps(body).encode(),headers={'Content-Type':'application/json'})
  with urllib.request.urlopen(q,timeout=180) as r:return json.load(r)
 cmd=['docker','exec',a.container,'curl','--fail','--silent','--show-error','--max-time','180',address+path]
 if body is not None:cmd+=['-H','Content-Type: application/json','--data-binary',json.dumps(body)]
 return json.loads(subprocess.check_output(cmd,text=True))
def wait_loaded(loaded):
 for _ in range(120):
  m=next((x for x in call('juiced/catalog')['data'] if x['id']==model),None)
  if m is None:time.sleep(.1);continue
  if m['loaded']==loaded:return m
  if m.get('operation',{}).get('state')=='failed':raise AssertionError(m['operation'])
  time.sleep(.1)
 raise AssertionError('Lifecycle timed out')
def reload():
 call('juiced/policy',{'model':model,'policy':'on-demand'})
 call('juiced/operation',{'model':model,'action':'unload'});wait_loaded(False)
 call('juiced/operation',{'model':model,'action':'load'});wait_loaded(True)
messages=[{'role':'user','content':'Once upon a time, a little girl and her cat lived in a blue house. '*3}]
body={'model':model,'messages':messages,'temperature':0,'seed':42,'max_tokens':24,'ignore_eos':True,'cache_prompt':True}
prior=next((x for x in call('juiced/catalog')['data'] if x['id']==model),None)
if prior and prior['loaded']:
 call('juiced/operation',{'model':model,'action':'unload'});wait_loaded(False)
if prior:
 assert prior['checkpoints']['main']==a.model_file, 'Use an isolated fixture server'
else:call('juiced/import/server',{'path':a.model_file,'name':model,'recipe':'llamacpp'})
call('models/'+model+'/options',{'llamacpp_backend':'cpu','ctx_size':128,'llamacpp_args':'-t 2 --parallel 1 '+('--chat-template-file '+shlex.quote(a.template_file) if a.template_file else '' if a.environment_template else '--chat-template chatml')+' --slot-save-path '+shlex.quote(a.snapshots)+' --cache-ram 256'})
call('juiced/policy',{'model':model,'policy':'on-demand'})
call('juiced/operation',{'model':model,'action':'load'});wait_loaded(True)
profile=call('juiced/cache/profile',{'model':model});assert profile['configured'],profile;address=profile['address'];proof={'profile':profile}
proof['cold']=engine(address,'/v1/chat/completions',{**body,'id_slot':0,'cache_prompt':False})
proof['saved']=engine(address,'/slots/0?action=save',{'filename':'engine-acceptance.bin'});assert proof['saved']['n_saved']>30
reload();profile2=call('juiced/cache/profile',{'model':model});assert profile2['fingerprint']==profile['fingerprint'];address=profile2['address']
proof['restored']=engine(address,'/slots/0?action=restore',{'filename':'engine-acceptance.bin'})
proof['reuse']=engine(address,'/v1/chat/completions',{**body,'id_slot':0});assert proof['reuse']['timings']['cache_n']>30,proof
assert proof['reuse']['choices'][0]['message']['content']==proof['cold']['choices'][0]['message']['content'],proof
call('juiced/cache/qualify',{'model':model,'fingerprint':profile2['fingerprint'],'max_snapshot_bytes':64*1024*1024,'evidence':'Real CPU engine save/reload/restore, >30 reused tokens and identical greedy output; test_engine_cache.py'})
call('juiced/cache',{'mode':'persistent','disk_budget_gib':1})
ollama_url=a.url.rstrip('/')+'/api/chat'
ollama_body={'model':model+':latest','messages':messages,'stream':True,'think':False,'options':{'num_ctx':128,'num_predict':8},'keep_alive':-1}
q=urllib.request.Request(ollama_url,data=json.dumps(ollama_body).encode(),headers={'Content-Type':'application/json'})
try:
 urllib.request.urlopen(q,timeout=30)
 raise AssertionError('Persistent model accepted an unleased protocol')
except urllib.error.HTTPError as e:
 assert e.code==409
def mcp_chat():
 q=urllib.request.Request(a.url.rstrip('/')+'/mcp',data=json.dumps({'jsonrpc':'2.0','id':1,'method':'tools/call','params':{'name':'lemonade_chat','arguments':{'model':model,'messages':messages,'max_tokens':4}}}).encode(),headers={'Content-Type':'application/json'})
 with urllib.request.urlopen(q,timeout=180) as r:return json.load(r)['result']
blocked=mcp_chat();assert blocked.get('isError') and 'persistent' in json.dumps(blocked),blocked
proof['mcp_persistent_rejected']=True
alias_url=a.url.rstrip('/')+'/internal/aliases'
alias_request=urllib.request.Request(alias_url,data=json.dumps({'alias':'Stories-Persistent-Alias','target':model}).encode(),headers={'Content-Type':'application/json'})
with urllib.request.urlopen(alias_request,timeout=30) as r:assert r.status==200
try:
 response_request=urllib.request.Request(base+'responses',data=json.dumps({'model':'Stories-Persistent-Alias','input':'A synthetic fixture.'}).encode(),headers={'Content-Type':'application/json'})
 try:
  urllib.request.urlopen(response_request,timeout=30)
  raise AssertionError('Alias bypassed persistent slot protection')
 except urllib.error.HTTPError as error:assert error.code==409
 proof['alias_unleased_rejected']=True
finally:
 with urllib.request.urlopen(urllib.request.Request(alias_url+'/Stories-Persistent-Alias',method='DELETE'),timeout=30) as r:assert r.status==200

call('juiced/cache',{'model_modes':{model:'memory'}})
with urllib.request.urlopen(q,timeout=180) as r:
 chunks=[json.loads(line) for line in r if line.strip()]
 assert chunks and chunks[-1]['done'] and any(x.get('message',{}).get('content') for x in chunks),chunks
assert call('juiced/cache')['settings']['mode']=='persistent'
allowed=mcp_chat();assert not allowed.get('isError'),allowed
proof['mcp_memory_allowed']=True
if a.container:
 subprocess.run(['docker','restart',a.container],check=True,stdout=subprocess.DEVNULL)
 for _ in range(120):
  try:
   settings=call('juiced/cache')['settings']
   assert settings['model_modes'][model]=='memory'
   break
  except (OSError,RuntimeError):time.sleep(.5)
 else:raise AssertionError('Container did not recover')
proof['ollama_memory_override']={'done':chunks[-1]['done'],'chunks':len(chunks),'global_mode_preserved':'persistent','restart_checked':bool(a.container)}
call('juiced/cache',{'model_modes':{}})
call('juiced/operation',{'model':model,'action':'load'});wait_loaded(True)
def ask(session,stream,cache_prompt=True,legacy=False,credential=None):
 q=urllib.request.Request(base+'chat/completions',data=json.dumps({**body,'cache_prompt':cache_prompt,'stream':stream,'stream_options':{'include_usage':True}}).encode(),headers={'Content-Type':'application/json',('X-Juiced-Thread-Id' if legacy else 'X-Juiced-Session-Id'):session,**(credential or {})})
 with urllib.request.urlopen(q,timeout=180) as r:
  record={'headers':dict(r.headers)}
  if not stream:record['response']=json.load(r)
  else:
   chunks=[]
   for line in r:
    if line.startswith(b'data:') and line[5:].strip()!=b'[DONE]':
     event=json.loads(line[5:]);assert not event.get('error'),event;chunks.append(event)
   record['chunks']=chunks
 return record
for streaming in [True,False]:
 session='engine-acceptance-'+('sse' if streaming else 'json')
 first=ask(session,streaming,credential={'Authorization':'Bearer anonymous-fixture-one'})
 for _ in range(50):
  saved=next((s for s in call('juiced/cache')['snapshots'] if s['session']==session),None)
  if saved:break
  time.sleep(.1)
 assert saved,call('juiced/cache')
 call('juiced/cache/retain', {'snapshot':saved['id'],'retained':True})
 reload();restored=ask(session,streaming,legacy=not streaming,credential={'x-api-key':'anonymous-fixture-two'});assert restored['headers'].get('X-Juiced-Cache')=='restored',restored
 data=restored['chunks'][-1] if streaming else restored['response'];assert data['timings']['cache_n']>30,restored
 before=next(x for x in call('juiced/cache')['snapshots'] if x['session']==session)
 assert before['retained'] is True
 assert 'messages' not in before and 'title' not in before
 opted_out=ask(session,streaming,False);assert opted_out['headers'].get('X-Juiced-Cache')=='off',opted_out
 data=opted_out['chunks'][-1] if streaming else opted_out['response'];assert data['timings']['cache_n']==0,opted_out
 after=next(x for x in call('juiced/cache')['snapshots'] if x['session']==session);assert after==before,(before,after)
 call('juiced/cache/forget',{'session':session})
 assert not any(s['session']==session for s in call('juiced/cache')['snapshots'])
 proof['sse' if streaming else 'json']={'first':first,'snapshot':saved,'restored':restored,'opted_out':opted_out}
call('juiced/cache',{'mode':'memory'})
Path(a.out).write_text(json.dumps(proof,indent=2)+'\n');print('PASS: engine restart reuse, durable SSE/JSON snapshots, cache opt-out, snapshot retention without conversation storage')
