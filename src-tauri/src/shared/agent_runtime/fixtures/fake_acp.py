"""Deterministic ACP v1 subprocess used by shared-core and daemon acceptance tests."""
import json,sys,threading,time,uuid,pathlib,os
lock=threading.Lock()
active=None
pending={}
cwd=pathlib.Path.cwd()
log=cwd/'fake-acp-requests.jsonl'
def emit(value):
 with lock:sys.stdout.write(json.dumps(value)+'\n');sys.stdout.flush()
def reply(i,result):emit({'jsonrpc':'2.0','id':i,'result':result})
def update(session,value):emit({'jsonrpc':'2.0','method':'session/update','params':{'sessionId':session,'update':value}})
def finish(job,cancelled=False):
 global active
 if job.get('done'):return
 job['done']=True
 update(job['session'],{'sessionUpdate':'tool_call_update','toolCallId':'tool-1','status':'completed','content':[{'type':'content','content':{'type':'text','text':'fixture output'}}]})
 reply(job['id'],{'stopReason':'cancelled' if cancelled else 'end_turn','usage':{'inputTokens':10,'outputTokens':3,'totalTokens':13}})
 active=None
for line in sys.stdin:
 request=json.loads(line)
 with log.open('a') as out:out.write(json.dumps(request)+'\n')
 method=request.get('method');i=request.get('id');p=request.get('params',{})
 if not method:
  job=pending.pop(str(i),None)
  if job:finish(job,request.get('result',{}).get('outcome',{}).get('outcome')=='cancelled')
 elif method=='initialize':reply(i,{'protocolVersion':2 if '--bad-version' in sys.argv else 1,'agentCapabilities':{'loadSession':'--no-load' not in sys.argv,'promptCapabilities':{'image':False}},'agentInfo':{'name':'Fixture','version':'1'}})
 elif method in ('session/new','session/load'):
  session=p.get('sessionId','native-'+str(uuid.uuid4()))
  if method=='session/load':update(session,{'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':'REPLAYED_LATER_RESULT'}})
  reply(i,{'sessionId':session,'configOptions':[{'id':'model','name':'Model','type':'select','currentValue':'fixture','options':[{'value':'fixture','name':'Fixture'}]}]})
 elif method=='session/set_config_option':reply(i,{'configOptions':[{'id':'model','name':'Model','type':'select','currentValue':p['value'],'options':[{'value':'fixture','name':'Fixture'}]}]})
 elif method=='session/prompt':
  text=''.join(c.get('text','') for c in p['prompt'])
  if 'disconnect' in text:os._exit(0)
  active={'id':i,'session':p['sessionId'],'done':False};job=active
  update('foreign-session',{'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':'FOREIGN_RESULT'}})
  update(p['sessionId'],{'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':text}})
  update(p['sessionId'],{'sessionUpdate':'tool_call','toolCallId':'tool-1','kind':'execute','title':'Fixture command','status':'pending','rawInput':{'command':'printf fixture'}})
  if 'permission' in text:
   pending['permission-1']=job
   emit({'jsonrpc':'2.0','id':'permission-1','method':'session/request_permission','params':{'sessionId':p['sessionId'],'toolCall':{'toolCallId':'tool-1','title':'Fixture command'},'options':[{'optionId':'once','name':'Allow once','kind':'allow_once'},{'optionId':'no','name':'Reject','kind':'reject_once'}]}})
  elif 'slow' in text:
   def delayed(j):time.sleep(2);finish(j)
   threading.Thread(target=delayed,args=(job,),daemon=True).start()
  else:finish(job)
 elif method=='session/cancel':
  if active:finish(active,True)
 elif i is not None:emit({'jsonrpc':'2.0','id':i,'error':{'code':-32601,'message':'Unsupported fixture method'}})
