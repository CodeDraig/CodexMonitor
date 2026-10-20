"""Independent ACP v1 fixture. Public protocol only; history lives in a test-owned JSON file.
Invoke with: python3 conversations_acp.py --catalog /absolute/catalog.json [--no-list] [--no-load]
"""
import json, pathlib, sys, time, threading, uuid
args=sys.argv[1:]
if 'acp' in args: raise SystemExit('generic launcher must not receive an appended acp argument')
source=pathlib.Path(args[args.index('--catalog')+1])
write_lock=threading.Lock()
def emit(v):
 with write_lock:
  print(json.dumps(v),flush=True)
def reply(i,result): emit({'jsonrpc':'2.0','id':i,'result':result})
def update(s,u):emit({'jsonrpc':'2.0','method':'session/update','params':{'sessionId':s,'update':u}})
def catalog():return json.loads(source.read_text())
for line in sys.stdin:
 v=json.loads(line);method=v.get('method');p=v.get('params',{});i=v.get('id')
 if method=='initialize':reply(i,{'protocolVersion':1,'agentInfo':{'name':'Independent ACP fixture','version':'1.0'},'agentCapabilities':{'loadSession':'--no-load' not in args,'sessionCapabilities':{} if '--no-list' in args else {'list':{}},'promptCapabilities':{'image':True,'audio':True,'embeddedContext':True}}})
 elif method=='session/list':
  rows=catalog()['sessions'];rows=[r for r in rows if not p.get('cwd') or r['cwd']==p['cwd']];offset=int(p.get('cursor','0'));page=rows[offset:offset+1]
  reply(i,{'sessions':[{k:v for k,v in s.items() if k not in ('updates','fail','slow','after')} for s in page],**({'nextCursor':str(offset+1)} if offset+1<len(rows) else {})})
 elif method=='session/load':
  session=p['sessionId'];row=next((s for s in catalog()['sessions'] if s['sessionId']==session),{})
  for u in row.get('updates',[]):update(session,u)
  if row.get('slow'):time.sleep(row['slow'])
  if row.get('fail'):emit({'jsonrpc':'2.0','id':i,'error':{'code':-32001,'message':'fixture failed after replay'}})
  else:reply(i,{'sessionId':session,'configOptions':[]})
  for u in row.get('after',[]):update(session,u)
 elif method=='session/new':
  # ACP creates a distinct conversation for each session/new request. Persist it
  # so prompts, discovery, and later session/load calls use the returned identity.
  session=f"new-fixture-{uuid.uuid4()}"
  doc=catalog();doc.setdefault('sessions',[]).append({'sessionId':session,'cwd':p['cwd'],'title':session,'updates':[]})
  source.write_text(json.dumps(doc))
  reply(i,{'sessionId':session,'configOptions':[]})
 elif method=='session/prompt':
  s=p['sessionId'];doc=catalog();row=next((r for r in doc['sessions'] if r['sessionId']==s),None)
  updates=[]
  for role in ('user_message_chunk','agent_message_chunk'):
   mid=str(uuid.uuid4())
   for c in p['prompt']:updates.append({'sessionUpdate':role,'messageId':mid,'content':c})
  if row is not None:
   row.setdefault('updates',[]).extend(updates);source.write_text(json.dumps(doc))
  for u in updates:update(s,u)
  reply(i,{'stopReason':'end_turn'})
 elif method=='session/cancel':pass
 elif i is not None:emit({'jsonrpc':'2.0','id':i,'error':{'code':-32601,'message':'unsupported fixture method'}})
