import {useCallback,useEffect,useMemo,useRef,useState} from "react";
import {runtimeRequest} from "@services/tauri";
import {MessageRow} from "@/features/messages/components/MessageRow";
import {ToolRow} from "@/features/messages/components/ToolRow";
import {useRuntimeConversation} from "../hooks/useRuntimeConversation";
import {projectRuntimeEvents} from "../projection";
import type {ExecutionTicket,RuntimeSession,RuntimeConfigOption} from "../types";
import {runtimeError} from "../types";
export function RuntimeConversation({session,backendKey,onConfiguration}:{session:RuntimeSession;backendKey:string;onConfiguration:(session:RuntimeSession)=>void}) {
  const {events,executions,error:readError,reload} = useRuntimeConversation(session,backendKey);
  const projected = useMemo(()=>projectRuntimeEvents(events),[events]);
  const configurationOptions:RuntimeConfigOption[] = session.configuration?.configOptions ?? [
    ...(session.configuration?.modes ? [{id:"mode",name:"Mode",type:"select",currentValue:session.configuration?.modes.currentModeId,options:session.configuration?.modes.availableModes.map(m=>({value:m.id,name:m.name}))}] : []),
    ...(session.configuration?.models ? [{id:"model",name:"Model",type:"select",currentValue:session.configuration?.models.currentModelId,options:session.configuration?.models.availableModels.map(m=>({value:m.modelId,name:m.name}))}] : []),
  ];
  const [followUp,setFollowUp] = useState<"queue"|"steer">("queue");
  const [draft,setDraft] = useState("");
  const [queue,setQueue] = useState<{requestId:string;text:string}[]>([]);
  const [error,setError] = useState<string|null>(null);
  const [uncertain,setUncertain] = useState<string|null>(null);
  const [awaitingExecution,setAwaitingExecution] = useState<string|null>(null);
  const [sending,setSending] = useState(false);
  const inFlight = useRef(false);
  const [expanded,setExpanded] = useState<Set<string>>(new Set());
  const active = executions.find(t=>["not_started","running","unknown"].includes(t.execution));
  const dispatch = useCallback(async (entry:{requestId:string;text:string})=>{
    if(inFlight.current)return;
    inFlight.current=true;setSending(true);setError(null);
    try {const receipt = await runtimeRequest<ExecutionTicket>("submit",{requestId:entry.requestId,sessionId:session.sessionId,prompt:[{type:"text",text:entry.text}],options:{}});setAwaitingExecution(receipt.executionId);reload();}
    catch(e){setError(runtimeError(e));setUncertain(entry.requestId);}
    finally{inFlight.current=false;setSending(false);}
  },[session.sessionId,reload]);
  useEffect(()=>{if(awaitingExecution && executions.some(t=>t.executionId === awaitingExecution && ["completed","failed","interrupted"].includes(t.execution))) setAwaitingExecution(null);},[awaitingExecution,executions]);
  useEffect(()=>{
    if(!awaitingExecution && !active && !sending && !uncertain && queue.length && !readError){const [next,...rest]=queue;setQueue(rest);void dispatch(next);}
  },[active,awaitingExecution,sending,uncertain,queue,readError,dispatch]);
  const [permissionBusy,setPermissionBusy]=useState(false);
  return <section className="run-workbench" aria-label="Runtime conversation">
    <p>{session.profile.name} · {String((session.agentInfo.agentInfo as {version?:string}|undefined)?.version ?? (session.agentInfo.initialization as {userAgent?:string}|undefined)?.userAgent ?? "version unavailable")} · {session.sessionId}</p>
    {configurationOptions.filter(o=>o.type==="select").map(option=><label key={option.id}>{option.name}<select aria-label={`Runtime ${option.name}`} value={option.currentValue} disabled={sending || !!active || !!awaitingExecution} onChange={async e=>{
      const value=e.target.value;setError(null);
      try {await runtimeRequest("configure",{sessionId:session.sessionId,optionId:option.id,value});const updated=await runtimeRequest<RuntimeSession>("load_session",{sessionId:session.sessionId});onConfiguration(updated);}
      catch(e){setError(runtimeError(e));}
    }}>{option.options.map((choice)=>"value" in choice?<option key={choice.value} value={choice.value}>{choice.name}</option>:<optgroup key={choice.name} label={choice.name}>{choice.options.map(c=><option key={c.value} value={c.value}>{c.name}</option>)}</optgroup>)}</select></label>)}
    {!session.capabilities.steering && <p>Follow-ups queue until the active execution finishes.</p>}
    {projected.items.map(item=>item.kind==="message"?<MessageRow key={item.id} item={item} isCopied={false} onCopy={i=>{void navigator.clipboard.writeText(i.text);}} workspacePath={session.cwd}/>:item.kind==="tool"?<ToolRow key={item.id} item={item} isExpanded={expanded.has(item.id)} onToggle={id=>setExpanded(previous=>{const next=new Set(previous);if(next.has(id))next.delete(id);else next.add(id);return next;})} workspacePath={session.cwd}/>:item.kind==="reasoning"?<details key={item.id}><summary>Reasoning</summary><pre>{item.content}</pre></details>:null)}
    {Object.entries(projected.permissions).map(([requestId,p])=><fieldset key={requestId} disabled={permissionBusy}><legend>Agent permission request</legend><pre>{JSON.stringify(p.toolCall ?? p,null,2)}</pre>{(p.options as {optionId:string;name:string}[]|undefined)?.map(option=><button type="button" className="ghost" key={option.optionId} onClick={async()=>{
      setPermissionBusy(true);setError(null);try{await runtimeRequest("permission",{sessionId:session.sessionId,requestId,optionId:option.optionId});reload();}catch(e){setError(runtimeError(e));}finally{setPermissionBusy(false);}
    }}>{option.name}</button>)}</fieldset>)}
    {(error || readError) && <p role="alert">{error ?? readError}</p>}
    {uncertain && <><p>Submission outcome needs checking. The prompt will not be resent automatically.</p><button type="button" className="ghost" onClick={async()=>{
      try{const receipt=await runtimeRequest<ExecutionTicket|null>("execution",{requestId:uncertain});if(receipt){setUncertain(null);setAwaitingExecution(receipt.executionId);setError(null);reload();}else setError("No durable receipt found. Keep this request ID for inspection: "+uncertain);}catch(e){setError(runtimeError(e));}
    }}>Check submission receipt</button></>}
    <form onSubmit={e=>{e.preventDefault();const text=draft.trim();if(!text || uncertain)return;const entry={requestId:crypto.randomUUID(),text};setDraft("");if(active && followUp === "steer" && session.capabilities.steering){setSending(true);void runtimeRequest("steer",{sessionId:session.sessionId,executionId:active.executionId,prompt:[{type:"text",text}]}).then(()=>reload()).catch(e=>{setError(runtimeError(e));setDraft(text);}).finally(()=>setSending(false));return;}if(active || awaitingExecution || sending)setQueue(previous=>[...previous,entry]);else void dispatch(entry);}}>
      {session.capabilities.steering && active && <label>Runtime follow-up<select value={followUp} onChange={e=>setFollowUp(e.target.value as "queue"|"steer")}><option value="queue">Queue</option><option value="steer">Steer active execution</option></select></label>}
      <label>Runtime message<textarea value={draft} onChange={e=>setDraft(e.target.value)} disabled={!!uncertain}/></label>
      <button className="primary" disabled={!draft.trim() || !!uncertain}>{active && followUp === "steer" && session.capabilities.steering ? "Steer runtime execution" : active || awaitingExecution || sending ? "Queue message" : "Send runtime message"}</button>
      {session.capabilities.cancellation && active && <button type="button" className="ghost" onClick={async()=>{setQueue([]);try{await runtimeRequest("cancel",{sessionId:session.sessionId});reload();}catch(e){setError(runtimeError(e));}}}>Stop runtime execution</button>}
    </form>
    {!!queue.length && <p role="status">{queue.length} queued messages</p>}
    {executions.map(t=><p key={t.executionId}>{t.executionId} · {t.submission} · {t.execution}{t.error ? ` · ${t.error}` : ""}</p>)}
  </section>;
}
