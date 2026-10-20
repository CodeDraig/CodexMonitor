import {useCallback,useEffect,useRef,useState} from "react";
import {openUrl} from "@tauri-apps/plugin-opener";
import {getRuntimeSessionConnectionStatus,respondAcpClientRequest} from "@services/tauri";
import type {AcpConnectionStatus} from "../types";
import {runtimeError} from "../types";

type ClientRequest={clientRequestId?:string|number;id?:string|number;method?:string;params?:Record<string,unknown>;[key:string]:unknown};
function requestId(request:ClientRequest){return request.clientRequestId??request.id;}
function approvedCallback(method:string){return method==="session/request_permission"||method==="fs/read_text_file"||method==="fs/write_text_file"||method==="terminal/create";}
function safeWebUrl(value:unknown):value is string{if(typeof value!=="string")return false;try{const url=new URL(value);return url.protocol==="https:"||url.protocol==="http:";}catch{return false;}}
function noLiveConnection(error:unknown){
  const text=runtimeError(error);
  const code=typeof error==="object"&&error!==null&&"code" in error?String(error.code):null;
  const transportLost=code!==null?code==="transport_lost":/^transport_lost:\s*/.test(text);
  return transportLost&&text.includes("live ACP connection for Monitor session not found");
}

/** Client callbacks for the selected conversation's already-consumed ACP connection. */
export function AcpSessionClientRequests({sessionId,backendKey,activeExecution=false}:{sessionId:string;backendKey:string;activeExecution?:boolean}){
  const [status,setStatus]=useState<AcpConnectionStatus|null>(null),[statusScope,setStatusScope]=useState<{backendKey:string;sessionId:string}|null>(null),[error,setError]=useState<string|null>(null),[waitingForConnection,setWaitingForConnection]=useState(false),[busyId,setBusyId]=useState<string|null>(null),[notice,setNotice]=useState<string|null>(null);
  const currentStatus=statusScope?.backendKey===backendKey&&statusScope.sessionId===sessionId?status:null;
  const epoch=useRef(0),backendKeyRef=useRef(backendKey),sessionIdRef=useRef(sessionId),activeExecutionRef=useRef(activeExecution);
  backendKeyRef.current=backendKey;sessionIdRef.current=sessionId;activeExecutionRef.current=activeExecution;
  const refresh=useCallback(async()=>{
    const version=epoch.current,selectedBackend=backendKey;
    try{
      const result=await getRuntimeSessionConnectionStatus(sessionId);
      if(version!==epoch.current||backendKeyRef.current!==selectedBackend||sessionIdRef.current!==sessionId)return;
      setStatus(result);setStatusScope({backendKey:selectedBackend,sessionId});setError(null);setWaitingForConnection(false);
    }catch(e){if(version===epoch.current&&backendKeyRef.current===selectedBackend&&sessionIdRef.current===sessionId){setStatus(null);setStatusScope(null);if(noLiveConnection(e)&&!activeExecutionRef.current){setError(null);setWaitingForConnection(true);}else{setWaitingForConnection(false);setError(runtimeError(e));}}}
  },[backendKey,sessionId]);
  useEffect(()=>{
    const version=++epoch.current;setStatus(null);setStatusScope(null);setError(null);setWaitingForConnection(false);setNotice(null);setBusyId(null);
    let alive=true,timer:ReturnType<typeof setTimeout>|undefined;
    const poll=async()=>{
      if(!alive||version!==epoch.current||backendKeyRef.current!==backendKey||sessionIdRef.current!==sessionId)return;
      try{const result=await getRuntimeSessionConnectionStatus(sessionId);
        if(alive&&version===epoch.current&&backendKeyRef.current===backendKey&&sessionIdRef.current===sessionId){setStatus(result);setStatusScope({backendKey,sessionId});setError(null);setWaitingForConnection(false);}}
      catch(e){if(alive&&version===epoch.current&&backendKeyRef.current===backendKey&&sessionIdRef.current===sessionId){setStatus(null);setStatusScope(null);if(noLiveConnection(e)&&!activeExecutionRef.current){setError(null);setWaitingForConnection(true);}else{setWaitingForConnection(false);setError(runtimeError(e));}}}
      if(alive&&version===epoch.current)timer=setTimeout(()=>void poll(),1500);
    };
    void poll();return()=>{alive=false;++epoch.current;if(timer)clearTimeout(timer);};
  },[backendKey,sessionId]);
  const respond=async(request:ClientRequest,action:"allow_once"|"deny"|"accept"|"decline"|"cancel")=>{
    const id=requestId(request);const connection=currentStatus;
    if(id===undefined||!connection||busyId!==null||backendKeyRef.current!==backendKey||sessionIdRef.current!==sessionId)return;
    const selectedBackend=backendKey;setBusyId(String(id));setError(null);setNotice(null);
    try{
      await respondAcpClientRequest({connectionId:connection.connectionId,generation:connection.generation,clientRequestId:id,decision:{action}});
      if(backendKeyRef.current===selectedBackend&&sessionIdRef.current===sessionId){setNotice(`Responded to ${request.method??"agent callback"}: ${action.replace(/_/g," ")}.`);await refresh();}
    }catch(e){if(backendKeyRef.current===selectedBackend&&sessionIdRef.current===sessionId)setError(runtimeError(e));}
    finally{if(backendKeyRef.current===selectedBackend&&sessionIdRef.current===sessionId)setBusyId(null);}
  };
  const requests=(currentStatus?.pendingClientRequests??[]) as ClientRequest[];
  return <details className="acp-client-requests" open={requests.length>0}>
    <summary>Client callback requests{requests.length?` · ${requests.length} pending`:""}</summary>
    <p>Requests belong to this ACP conversation. Responses are sent to its current backend connection.</p>
    {!currentStatus&&!error&&(waitingForConnection?<p role="status">Waiting for the foreground ACP connection.</p>:<p role="status">Checking the existing ACP connection…</p>)}
    {currentStatus&&<p role="status">Connected as {currentStatus.profileId} · generation {currentStatus.generation}</p>}
    {requests.map(request=>{
      const id=requestId(request);const idKey=String(id);const method=String(request.method??"unknown client request");
      const params=request.params??{};const url=request.url??params.url;const message=request.message??params.message;const title=request.title??params.title;const isElicitation=request.kind==="url"||method.startsWith("elicitation/");const isApproval=request.kind==="approval"||approvedCallback(method);
      return <article key={idKey}>
        <strong>{method}</strong>
        {typeof message==="string"&&<p>{message}</p>}
        {typeof title==="string"&&<p>{title}</p>}
        {typeof request.command==="string"&&<p>Command: <code>{request.command}</code></p>}
        {Array.isArray(request.args)&&<p>Arguments: <code>{request.args.map(String).join(" ")}</code></p>}
        {typeof request.cwd==="string"&&<p>Working directory: <code>{request.cwd}</code></p>}
        {url!==undefined&&<p className="acp-client-request-destination"><span>Destination:</span> <code>{String(url)}</code></p>}
        {safeWebUrl(url)&&<button type="button" className="ghost" onClick={()=>void openUrl(url).catch(e=>setError(runtimeError(e)))}>Open destination</button>}
        {isElicitation?<><button type="button" className="ghost" disabled={busyId!==null} onClick={()=>void respond(request,"accept")}>Confirm consent</button><button type="button" className="ghost" disabled={busyId!==null} onClick={()=>void respond(request,"decline")}>Decline consent</button></>:isApproval?<><button type="button" className="ghost" disabled={busyId!==null} onClick={()=>void respond(request,"allow_once")}>Allow once</button><button type="button" className="ghost" disabled={busyId!==null} onClick={()=>void respond(request,"deny")}>Deny request</button></>:<p>This callback has no approval control in this compatibility view.</p>}
        <button type="button" className="ghost" disabled={busyId!==null} onClick={()=>void respond(request,"cancel")}>Cancel request</button>
        <details><summary>Inspect callback payload</summary><pre>{JSON.stringify(request,null,2)}</pre></details>
      </article>;
    })}
    {notice&&<p role="status">{notice}</p>}{error&&<p role="alert">{error}</p>}
    <button type="button" className="ghost" onClick={()=>void refresh()}>Refresh callback requests</button>
  </details>;
}
