import {RuntimeProfileEditor} from "./RuntimeProfileEditor";
import {useEffect,useRef,useState} from "react";
import {ModalShell} from "@/features/design-system/components/modal/ModalShell";
import {runtimeRequest} from "@services/tauri";
import type {AcpConversations} from "../hooks/useAcpConversations";
import {runtimeError,type DiscoveryPage,type CatalogSession} from "../types";
export function AgentSessionBrowser({runtime}:{runtime:AcpConversations}){
  const [profile,setProfile]=useState("");const [query,setQuery]=useState("");
  const [sources,setSources]=useState<Record<string,{status:string;cursor:string|null;busy:boolean}>>({});
  const [error,setError]=useState<string|null>(null);
  const [nativeId,setNativeId]=useState("");const [cwd,setCwd]=useState("");
  const card=useRef<HTMLDivElement>(null),epoch=useRef(0);
  useEffect(()=>{++epoch.current;return()=>{++epoch.current;};},[runtime.backendKey]);
  useEffect(()=>{
    if(!runtime.browserOpen)return;
    const previous=document.activeElement as HTMLElement|null;card.current?.querySelector<HTMLInputElement>("input")?.focus();
    const key=(e:KeyboardEvent)=>{if(e.key==="Escape")runtime.setBrowserOpen(false);if(e.key==="Tab"){
      const nodes=Array.from(card.current?.querySelectorAll<HTMLElement>('button:not(:disabled),input,select,[tabindex="0"]')??[]);
      const first=nodes[0],last=nodes[nodes.length-1];if(e.shiftKey&&document.activeElement===first){e.preventDefault();last?.focus();}else if(!e.shiftKey&&document.activeElement===last){e.preventDefault();first?.focus();}
    }};document.addEventListener("keydown",key);return()=>{document.removeEventListener("keydown",key);previous?.focus();};
  },[runtime.browserOpen,runtime.setBrowserOpen]);
  const scan=async(id:string,more=false)=>{
    const version=epoch.current;setSources(p=>({...p,[id]:{status:"Discovering…",cursor:more?p[id]?.cursor??null:null,busy:true}}));
    try{const result=await runtimeRequest<DiscoveryPage>("discover_sessions",{profileId:id,cursor:more?sources[id]?.cursor:undefined});if(version!==epoch.current)return;
      setSources(p=>({...p,[id]:{status:result.sourceStatus==="unsupported"?`Known sessions only · discovery unavailable${result.capabilities.loadSession?" · open by ID supported":" · history loading unavailable"}`:`Discovery available · ${result.capabilities.loadSession?"history replay available":"history loading unavailable"}`,cursor:result.nextCursor,busy:false}}));await runtime.refreshCatalog();
    }catch(e){if(version===epoch.current)setSources(p=>({...p,[id]:{status:runtimeError(e),cursor:null,busy:false}}));}
  };
  const open=async(row:CatalogSession)=>{setError(null);try{await runtime.open(row,!runtime.hasWorkspace(row.cwd,row.workspaceId));}catch(e){setError(runtimeError(e));}};
  if(!runtime.browserOpen)return <button type="button" className="ghost" onClick={()=>runtime.setBrowserOpen(true)}>Browse agent sessions</button>;
  return <ModalShell ariaLabel="Browse agent sessions" onBackdropClick={()=>runtime.setBrowserOpen(false)}><div ref={card} className="agent-session-browser">
    <h2>Browse agent sessions</h2><button type="button" className="ghost" onClick={()=>runtime.setBrowserOpen(false)}>Close</button>
    <label>Search title or directory<input value={query} onChange={e=>setQuery(e.target.value)}/></label>
    <label>Agent profile<select value={profile} onChange={e=>setProfile(e.target.value)}><option value="">All ACP profiles</option>{runtime.profiles.filter(p=>p.kind!=="codex").map(p=><option key={p.id} value={p.id}>{p.name}</option>)}</select></label>
    {runtime.profiles.filter(p=>p.kind!=="codex"&&(!profile||p.id===profile)).map(p=><section key={p.id}><h3>{p.name}</h3><p role="status">{sources[p.id]?.status??runtime.sourceErrors[p.id]??"Cached sessions · discover to check agent capabilities"}</p><button className="ghost" type="button" disabled={sources[p.id]?.busy} onClick={()=>void scan(p.id)}>Discover sessions</button>{sources[p.id]?.cursor&&<button className="ghost" type="button" disabled={sources[p.id]?.busy} onClick={()=>void scan(p.id,true)}>More from {p.name}</button>}</section>)}
    <p>Directories are resolved on the selected backend. Failed sources keep their cached sessions.</p>
    <ul>{runtime.catalog.filter(r=>r.profile.kind!=="codex"&&(!profile||r.profile.id===profile)&&`${r.title??""} ${r.presentation.titleOverride??""} ${r.cwd}`.toLowerCase().includes(query.toLowerCase())).map(row=><li key={row.catalogId}><strong>{row.profile.name} · {row.presentation.titleOverride??row.title??row.nativeSessionId}</strong><p>{row.cwd}{row.presentation.hidden?" · Hidden in sidebar":""}</p><button className="ghost" type="button" onClick={()=>void open(row)}>{runtime.hasWorkspace(row.cwd,row.workspaceId)?"Open conversation":"Add workspace and open"}</button>{row.presentation.hidden&&row.sessionId&&<button type="button" className="ghost" onClick={()=>void runtime.metadata(`acp:v1:${encodeURIComponent(runtime.backendKey)}:${encodeURIComponent(row.storeId)}:${encodeURIComponent(row.sessionId!)}`,{hidden:false}).catch(e=>setError(runtimeError(e)))}>Show in sidebar</button>}</li>)}</ul>
    <RuntimeProfileEditor backendKey={runtime.backendKey} profiles={runtime.profiles} onSaved={runtime.refreshCatalog}/>
    <details><summary>Open session by ID</summary><p>Requires the selected agent to support session loading.</p><form onSubmit={e=>{e.preventDefault();setError(null);const version=epoch.current;void runtimeRequest<CatalogSession>("open_session_by_id",{profileId:profile,nativeSessionId:nativeId,cwd}).then(row=>version===epoch.current?runtime.open(row,true):undefined).catch(e=>{if(version===epoch.current)setError(runtimeError(e));});}}><label>Native session ID<input value={nativeId} onChange={e=>setNativeId(e.target.value)}/></label><label>Agent directory<input value={cwd} onChange={e=>setCwd(e.target.value)}/></label><button className="ghost" disabled={!profile||!nativeId||!cwd}>Add workspace and open</button></form></details>
    {error&&<p role="alert">{error}</p>}
  </div></ModalShell>;
}
