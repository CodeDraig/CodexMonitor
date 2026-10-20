import {useCallback,useEffect,useRef,useState} from "react";
import {openUrl} from "@tauri-apps/plugin-opener";
import {
  authenticateRuntimeConnection, cancelRuntimeConnectionOperation, closeAcpLoginTerminal,
  closeRuntimeConnection,getRuntimeConnectionOperation, getRuntimeConnectionStatus, logoutRuntimeConnection,
  prepareRuntimeConnection, readAcpLoginTerminal, respondAcpClientRequest,
  resizeAcpLoginTerminal, writeAcpLoginTerminal,
  type RuntimeConnectionOperation,
} from "@services/tauri";
import type {AcpAuthMethod,RuntimeProfile} from "../types";
import {runtimeError} from "../types";

type ClientRequest={id?:string|number;clientRequestId?:string|number;method?:string;kind?:string;url?:string;message?:string;description?:string;params?:Record<string,unknown>;[key:string]:unknown};
type ConnectionStatus={connectionId:string;generation:number;profileId:string;agentInfo:Record<string,unknown>;capabilities:Record<string,unknown>;authMethods:AcpAuthMethod[];authState:string;pendingClientRequests:ClientRequest[]};
const finalStates=new Set(["succeeded","failed","cancelled","interrupted"]);
function utf8Base64(text:string){const bytes=new TextEncoder().encode(text);let binary="";for(const byte of bytes)binary+=String.fromCharCode(byte);return btoa(binary);}

export function AcpConnectionControls({profile,backendKey="local"}:{profile:RuntimeProfile;backendKey?:string}){
  const [connection,setConnection]=useState<{connectionId:string;generation:number;backendKey:string}|null>(null);
  const [status,setStatus]=useState<ConnectionStatus|null>(null);
  const [operation,setOperation]=useState<RuntimeConnectionOperation|null>(null);
  const [activeAuthType,setActiveAuthType]=useState("");
  const [terminalOutput,setTerminalOutput]=useState("");const [terminalCursor,setTerminalCursor]=useState(0);const [terminalInput,setTerminalInput]=useState("");
  const [busy,setBusy]=useState(false),[error,setError]=useState<string|null>(null),[notice,setNotice]=useState<string|null>(null);
  const inputSequence=useRef(0),generationRef=useRef(0),backendKeyRef=useRef(backendKey);
  const connectionRef=useRef(connection),operationRef=useRef(operation),operationPurposeRef=useRef<"prepare"|"authenticate"|"logout"|null>(null);
  backendKeyRef.current=backendKey;connectionRef.current=connection;operationRef.current=operation;
  const disposeConnection=useCallback(async(ownerBackendKey:string,owned:{connectionId:string;generation:number},currentOperation?:RuntimeConnectionOperation|null)=>{
    if(backendKeyRef.current!==ownerBackendKey)return false;
    if(currentOperation&&!finalStates.has(currentOperation.state)){
      try{await cancelRuntimeConnectionOperation({operationId:currentOperation.operationId,connectionId:owned.connectionId,generation:owned.generation,requestId:crypto.randomUUID()});}catch{/* Close remains the backend's final ownership check. */}
    }
    await closeRuntimeConnection({connectionId:owned.connectionId,generation:owned.generation});return true;
  },[]);
  const closeOwnedConnection=useCallback(async(ownerBackendKey:string):Promise<boolean>=>{
    const owned=connectionRef.current;
    // The global runtime router always targets the currently selected backend.
    // Never send an old connection ID after that selection changes.
    if(!owned||owned.backendKey!==ownerBackendKey||backendKeyRef.current!==ownerBackendKey)return false;
    return disposeConnection(ownerBackendKey,owned,operationRef.current);
  },[disposeConnection]);
  const refreshStatus=useCallback(async()=>{
    if(!connection||connection.backendKey!==backendKeyRef.current)return;
    const current=generationRef.current;
    const next=await getRuntimeConnectionStatus(connection.connectionId,connection.generation);
    if(current===generationRef.current)setStatus(next as ConnectionStatus);
  },[connection]);
  useEffect(()=>{
    const ownerBackendKey=backendKey;
    generationRef.current++;setConnection(null);setStatus(null);setOperation(null);setActiveAuthType("");setError(null);setNotice(null);setTerminalOutput("");setTerminalCursor(0);operationPurposeRef.current=null;
    return()=>{generationRef.current++;void closeOwnedConnection(ownerBackendKey).catch(()=>{});};
  },[profile.id,profile.revision,backendKey,closeOwnedConnection]);
  useEffect(()=>{
    if(!connection||connection.backendKey!==backendKeyRef.current)return;
    let alive=true;let timer:ReturnType<typeof setTimeout>|undefined;
    const poll=async()=>{
      if(!alive)return;
      try{const next=await getRuntimeConnectionStatus(connection.connectionId,connection.generation);if(!alive)return;setStatus(next as ConnectionStatus);}
      catch(e){if(alive)setError(runtimeError(e));}
      if(alive)timer=setTimeout(()=>void poll(),2500);
    };
    void poll();return()=>{alive=false;if(timer)clearTimeout(timer);};
  },[connection]);
  useEffect(()=>{
    if(!operation||!connection||connection.backendKey!==backendKeyRef.current||finalStates.has(operation.state))return;
    let alive=true;let timer:ReturnType<typeof setTimeout>|undefined;
    const poll=async()=>{
      if(!alive)return;
      try{const next=await getRuntimeConnectionOperation({operationId:operation.operationId,connectionId:operation.connectionId,generation:operation.generation});
        if(!alive)return;setOperation(next);if(finalStates.has(next.state)){if(operationPurposeRef.current==="prepare"&&next.state==="succeeded")operationPurposeRef.current=null;await refreshStatus();return;}}
      catch(e){if(alive)setError(runtimeError(e));}
      if(alive)timer=setTimeout(()=>void poll(),750);
    };
    timer=setTimeout(()=>void poll(),200);return()=>{alive=false;if(timer)clearTimeout(timer);};
  },[operation,connection,refreshStatus]);
  useEffect(()=>{
    if(!connection||connection.backendKey!==backendKeyRef.current||operation?.state!=="awaiting_user")return;
    if(activeAuthType!=="terminal")return;
    let alive=true;let timer:ReturnType<typeof setTimeout>|undefined;
    const poll=async()=>{if(!alive)return;try{const result=await readAcpLoginTerminal({connectionId:connection.connectionId,generation:connection.generation,operationId:operation.operationId,cursor:terminalCursor,limit:4096});
      if(alive){if(result.output)setTerminalOutput(previous=>(previous+result.output).slice(-32000));setTerminalCursor(result.nextCursor);}}
      catch(e){if(alive)setError(runtimeError(e));}
      if(alive)timer=setTimeout(()=>void poll(),500);
    };
    void poll();return()=>{alive=false;if(timer)clearTimeout(timer);};
  },[connection,operation,activeAuthType,terminalCursor]);

  const prepare=async()=>{
    setBusy(true);setError(null);setNotice(null);setOperation(null);setStatus(null);
    const expectedGeneration=generationRef.current,ownerBackendKey=backendKey;
    try{const result=await prepareRuntimeConnection({profileId:profile.id,expectedProfileRevision:profile.revision,cwd:profile.workingDirectory??undefined,role:"authentication",requestId:crypto.randomUUID()});
      if(expectedGeneration!==generationRef.current){void disposeConnection(ownerBackendKey,result,result);return;}
      operationPurposeRef.current="prepare";setConnection({connectionId:result.connectionId,generation:result.generation,backendKey:ownerBackendKey});setOperation(result);}
    catch(e){if(expectedGeneration===generationRef.current)setError(runtimeError(e));}finally{if(expectedGeneration===generationRef.current)setBusy(false);}
  };
  const authenticate=async(method:AcpAuthMethod)=>{
    if(!connection||connection.backendKey!==backendKeyRef.current)return;setBusy(true);setError(null);setNotice(null);setTerminalOutput("");setTerminalCursor(0);
    const expectedGeneration=generationRef.current,ownerBackendKey=connection.backendKey;
    operationPurposeRef.current="authenticate";setActiveAuthType(String(method.type??""));
    try{const result=await authenticateRuntimeConnection({connectionId:connection.connectionId,generation:connection.generation,methodId:method.id,requestId:crypto.randomUUID()});if(expectedGeneration!==generationRef.current){void disposeConnection(ownerBackendKey,connection,result);return;}setOperation(result);}
    catch(e){if(expectedGeneration===generationRef.current)setError(runtimeError(e));}finally{if(expectedGeneration===generationRef.current)setBusy(false);}
  };
  const respond=async(request:ClientRequest,action:"allow_once"|"deny"|"accept"|"decline"|"cancel")=>{
    if(!connection||connection.backendKey!==backendKeyRef.current)return;const requestId=request.clientRequestId??request.id;if(requestId===undefined)return;setError(null);try{await respondAcpClientRequest({connectionId:connection.connectionId,generation:connection.generation,clientRequestId:requestId,decision:{action}});setNotice(`Responded to ${request.method??request.kind??"agent request"}: ${action.replace(/_/g," ")}.`);await refreshStatus();}catch(e){setError(runtimeError(e));}
  };
  const sendTerminalInput=async()=>{
    if(!connection||connection.backendKey!==backendKeyRef.current||!operation||!terminalInput)return;
    const text=terminalInput;setTerminalInput("");const next=inputSequence.current+1;
    try{await writeAcpLoginTerminal({connectionId:connection.connectionId,generation:connection.generation,operationId:operation.operationId,inputSequence:next,data:utf8Base64(text+"\n")});inputSequence.current=next;}
    catch(e){setTerminalInput(text);setError(runtimeError(e));}
  };
  const cancel=async()=>{
    if(!connection||connection.backendKey!==backendKeyRef.current||!operation)return;setError(null);
    try{const next=await cancelRuntimeConnectionOperation({operationId:operation.operationId,connectionId:connection.connectionId,generation:connection.generation,requestId:crypto.randomUUID()});setOperation(next);}
    catch(e){setError(runtimeError(e));}
  };
  const logout=async()=>{
    if(!connection||connection.backendKey!==backendKeyRef.current)return;setBusy(true);setError(null);
    try{operationPurposeRef.current="logout";const result=await logoutRuntimeConnection({connectionId:connection.connectionId,generation:connection.generation,requestId:crypto.randomUUID()});setOperation(result);}
    catch(e){setError(runtimeError(e));}finally{setBusy(false);}
  };
  const disconnect=async()=>{
    if(!connection||connection.backendKey!==backendKeyRef.current)return;setBusy(true);setError(null);setNotice(null);
    try{const closed=await closeOwnedConnection(connection.backendKey);if(!closed)throw new Error("This connection belongs to a backend that is no longer selected.");setConnection(null);setStatus(null);setOperation(null);setActiveAuthType("");operationPurposeRef.current=null;setNotice("Disconnected from the unused ACP connection.");}
    catch(e){setError(runtimeError(e));}finally{setBusy(false);}
  };
  const terminalAuth=operation?.state==="awaiting_user"&&activeAuthType==="terminal";
  const canLogout=Boolean(status?.capabilities.logout||status?.capabilities.authLogout);
  const authMethods=status?.authMethods??[];
  return <section aria-label={`${profile.name} connection and authentication`}>
    <h3>Connection and sign-in</h3>
    {!connection||connection.backendKey!==backendKey?<button type="button" className="ghost" disabled={busy} onClick={()=>void prepare()}>Connect {profile.name}</button>:<>
      <p role="status">{status?`${String(status.agentInfo.name??profile.name)} · ${status.authState} · connection generation ${connection.generation}`:`Connecting to ${profile.name}…`}</p>
      {authMethods.length>0&&<fieldset disabled={busy}><legend>Authentication methods</legend>{authMethods.map(method=><button type="button" className="ghost" key={method.id} onClick={()=>void authenticate(method)}>{method.name||method.id}</button>)}</fieldset>}
      {status&&authMethods.length===0&&<p>No authentication method is currently required or advertised.</p>}
      {operation&&!finalStates.has(operation.state)&&<p role="status">{operation.state.replace(/_/g," ")}</p>}
      {operation&&!finalStates.has(operation.state)&&<button type="button" className="ghost" onClick={()=>void cancel()}>Cancel connection operation</button>}
      {terminalAuth&&<div aria-label="Agent login terminal">
        <p>Complete sign-in on the machine running this agent ({profile.workingDirectory||"backend environment"}). Login output is temporary and is not saved with conversations.</p>
        <pre role="log" aria-live="polite">{terminalOutput||"Waiting for login output…"}</pre>
        <form onSubmit={e=>{e.preventDefault();void sendTerminalInput();}}><label>Login input<input aria-label="Login terminal input" value={terminalInput} onChange={e=>setTerminalInput(e.target.value)} autoComplete="off"/></label><button className="ghost" disabled={!terminalInput}>Send login input</button></form>
        <button className="ghost" type="button" onClick={()=>void resizeAcpLoginTerminal({connectionId:connection.connectionId,generation:connection.generation,operationId:operation!.operationId,columns:100,rows:30}).catch(e=>setError(runtimeError(e)))}>Set terminal size</button>
        <button className="ghost" type="button" onClick={()=>void closeAcpLoginTerminal({connectionId:connection.connectionId,generation:connection.generation,operationId:operation!.operationId}).catch(e=>setError(runtimeError(e)))}>Close login terminal</button>
      </div>}
      {status?.pendingClientRequests?.map(request=>{
        const rawUrl=request.url??request.params?.url,rawMessage=request.message??request.params?.message;
        const url=typeof rawUrl==="string"?rawUrl:null;
        const isElicitation=String(request.method??request.kind??"").includes("elicitation");
        return <article key={String(request.clientRequestId??request.id)}>
          <p>{String(rawMessage??request.description??request.method??"The agent requested an action")}</p>
          {url&&<><p>{url}</p><button type="button" className="ghost" onClick={()=>void openUrl(url).catch(e=>setError(runtimeError(e)))}>Open sign-in URL</button></>}
          {isElicitation?<><button type="button" className="ghost" onClick={()=>void respond(request,"accept")}>Confirm URL sign-in</button><button type="button" className="ghost" onClick={()=>void respond(request,"decline")}>Decline URL sign-in</button></>:<><button type="button" className="ghost" onClick={()=>void respond(request,"allow_once")}>Allow once</button><button type="button" className="ghost" onClick={()=>void respond(request,"deny")}>Deny request</button></>}
          <details><summary>Request details</summary><pre>{JSON.stringify(request,null,2)}</pre></details>
        </article>;
      })}
      {canLogout&&<button type="button" className="ghost" disabled={busy} onClick={()=>void logout()}>Sign out {profile.name}</button>}
      <button type="button" className="ghost" disabled={busy} onClick={()=>void disconnect()}>Disconnect idle ACP connection</button>
    </>}
    {notice&&<p role="status">{notice}</p>}{error&&<p role="alert">{error}</p>}
  </section>;
}
