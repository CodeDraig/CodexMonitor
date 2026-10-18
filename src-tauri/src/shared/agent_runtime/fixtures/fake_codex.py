#!/usr/bin/env python3
"""Codex transport fixture with unique turns and output before the submission reply."""
import json,sys,time
if 'app-server' not in sys.argv:
 print('codex-cli 0.162.1');sys.exit(0)
count=0
def emit(v):print(json.dumps(v),flush=True)
for line in sys.stdin:
 r=json.loads(line)
 if 'id' not in r:continue
 p=r.get('params') or {};m=r.get('method');result={}
 if m=='initialize':result={'userAgent':'fixture'}
 elif m in ('thread/start','thread/resume'):result={'thread':{'id':'thread-1'}}
 elif m=='turn/start':
  count+=1;turn='turn-'+str(count);text=''.join(v.get('text','') for v in p.get('input',[]))
  for method,params in [('turn/started',{'turn':{'id':turn,'status':'inProgress'}}),('item/completed',{'turnId':turn,'item':{'id':'item-'+str(count),'type':'agentMessage','text':text}})]:emit({'method':method,'params':{'threadId':'thread-1',**params}})
  if 'slow' in text:time.sleep(2)
  emit({'method':'turn/completed','params':{'threadId':'thread-1','turn':{'id':turn,'status':'completed'}}})
  result={'turn':{'id':turn}}
 emit({'id':r['id'],'result':result})
 if m in ('thread/start','thread/resume'):emit({'method':'thread/started','params':{'thread':{'id':'thread-1'}}})
