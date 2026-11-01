#!/usr/bin/env python3
"""Real engine regression gate: useful restore, SSE/JSON snapshots and forget.

Uses ggml-org's 19 MB Stories15M Q4_0 fixture. This checks serving/state behavior,
not model quality. The model file must already be present in the server namespace.
"""
import argparse,json,subprocess,time,urllib.request,urllib.error
from pathlib import Path
p=argparse.ArgumentParser();p.add_argument('--url',required=True);p.add_argument('--model-file',required=True);p.add_argument('--snapshots',required=True);p.add_argument('--container');p.add_argument('--out',required=True);a=p.parse_args();base=a.url.rstrip('/')+'/v1/';model='Stories15M-Q4_0'
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
call('models/'+model+'/options',{'llamacpp_backend':'cpu','ctx_size':128,'llamacpp_args':'-t 2 --parallel 1 --chat-template chatml --slot-save-path '+a.snapshots+' --cache-ram 256'})
call('juiced/operation',{'model':model,'action':'load'});wait_loaded(True)
profile=call('juiced/cache/profile',{'model':model});assert profile['configured'],profile;address=profile['address'];proof={'profile':profile}
proof['cold']=engine(address,'/v1/chat/completions',{**body,'id_slot':0,'cache_prompt':False})
proof['saved']=engine(address,'/slots/0?action=save',{'filename':'engine-acceptance.bin'});assert proof['saved']['n_saved']>30
reload();profile2=call('juiced/cache/profile',{'model':model});assert profile2['fingerprint']==profile['fingerprint'];address=profile2['address']
proof['restored']=engine(address,'/slots/0?action=restore',{'filename':'engine-acceptance.bin'})
proof['reuse']=engine(address,'/v1/chat/completions',{**body,'id_slot':0});assert proof['reuse']['timings']['cache_n']>30,proof
assert proof['reuse']['choices'][0]['message']['content']==proof['cold']['choices'][0]['message']['content'],proof
call('juiced/cache/qualify',{'model':model,'fingerprint':profile2['fingerprint'],'evidence':'Real CPU engine save/reload/restore, >30 reused tokens and identical greedy output; test_engine_cache.py'})
call('juiced/cache',{'mode':'persistent','disk_budget_gib':1})
def ask(thread,stream,cache_prompt=True):
 q=urllib.request.Request(base+'chat/completions',data=json.dumps({**body,'cache_prompt':cache_prompt,'stream':stream,'stream_options':{'include_usage':True}}).encode(),headers={'Content-Type':'application/json','X-Juiced-Thread-Id':thread})
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
 thread='engine-acceptance-'+('sse' if streaming else 'json')
 existing=call('juiced/thread?id='+thread)
 call('juiced/thread',{'revision':existing.get('revision',0),'id':thread,'title':'Engine acceptance','model':model,'messages':messages})
 first=ask(thread,streaming)
 for _ in range(50):
  saved=next((s for s in call('juiced/cache')['snapshots'] if s['thread']==thread),None)
  if saved:break
  time.sleep(.1)
 assert saved,call('juiced/cache')
 reload();restored=ask(thread,streaming);assert restored['headers'].get('X-Juiced-Cache')=='restored',restored
 data=restored['chunks'][-1] if streaming else restored['response'];assert data['timings']['cache_n']>30,restored
 before=next(x for x in call('juiced/cache')['snapshots'] if x['thread']==thread)
 opted_out=ask(thread,streaming,False);assert opted_out['headers'].get('X-Juiced-Cache')=='off',opted_out
 data=opted_out['chunks'][-1] if streaming else opted_out['response'];assert data['timings']['cache_n']==0,opted_out
 after=next(x for x in call('juiced/cache')['snapshots'] if x['thread']==thread);assert after==before,(before,after)
 call('juiced/cache/forget',{'thread':thread})
 assert not any(s['thread']==thread for s in call('juiced/cache')['snapshots'])
 assert call('juiced/thread?id='+thread)['messages']==messages
 proof['sse' if streaming else 'json']={'first':first,'snapshot':saved,'restored':restored,'opted_out':opted_out}
call('juiced/cache',{'mode':'memory'})
Path(a.out).write_text(json.dumps(proof,indent=2)+'\n');print('PASS: engine restart reuse, durable SSE/JSON snapshots, cache opt-out, forget preserves messages')
