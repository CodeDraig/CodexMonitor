import {useCallback,useEffect,useRef,useState} from "react";
import {runtimeRequest} from "@services/tauri";
import type {RuntimeProfile,RuntimeSession} from "../types";
import {runtimeError} from "../types";
import {RuntimeProfileEditor} from "./RuntimeProfileEditor";
export function RuntimePanel({workspaceId,backendKey,requestedSessionId,onRunSelection}:{workspaceId:string;backendKey:string;requestedSessionId?:string|null;onRunSelection?:(profiles:RuntimeProfile[])=>void}) {
  const [profiles,setProfiles]=useState<RuntimeProfile[]>([]),[selected,setSelected]=useState<string[]>([]),[sessions,setSessions]=useState<RuntimeSession[]>([]);
  const [profileId,setProfileId]=useState("codex"),[error,setError]=useState<string|null>(null),[busy,setBusy]=useState(false);
  const generation=useRef(0);
  const load=useCallback(async()=>{const version=generation.current;const [result,known]=await Promise.all([runtimeRequest<{profiles:RuntimeProfile[]}>("profiles"),runtimeRequest<RuntimeSession[]>("sessions",{workspaceId})]);if(version===generation.current){setProfiles(result.profiles);setSessions(known);}},[workspaceId,backendKey]);
  const open=useCallback((sessionId:string)=>window.dispatchEvent(new CustomEvent("runtime-open-conversation",{detail:{backendKey,sessionId}})),[backendKey]);
  useEffect(()=>{generation.current++;setSelected([]);setError(null);setProfiles([]);setSessions([]);void load().catch(e=>setError(runtimeError(e)));return()=>{generation.current++;};},[load]);
  useEffect(()=>{if(requestedSessionId)open(requestedSessionId);},[requestedSessionId,open]);
  return <section className="run-workbench" aria-label="Agent runtimes"><h2>Agent runtimes</h2><p>Select agents for recorded runs. Leave the selection empty to use the Codex composer settings.</p>
    <fieldset disabled={busy}><legend>Agents for recorded runs</legend>{profiles.map(profile=><label key={profile.id}><input type="checkbox" checked={selected.includes(profile.id)} onChange={e=>{const ids=e.target.checked?[...selected,profile.id]:selected.filter(id=>id!==profile.id);setSelected(ids);onRunSelection?.(profiles.filter(p=>ids.includes(p.id)));}}/>{profile.name}</label>)}</fieldset>
    <RuntimeProfileEditor profiles={profiles} onSaved={load}/>
    <label>Conversation agent<select value={profileId} onChange={e=>setProfileId(e.target.value)} disabled={busy}>{profiles.map(p=><option key={p.id} value={p.id}>{p.name}</option>)}</select></label>
    <button className="ghost" type="button" disabled={busy||!profiles.length} onClick={async()=>{setBusy(true);setError(null);const version=generation.current;try{const s=await runtimeRequest<RuntimeSession>("create_session",{workspaceId,profileId,expectedProfileRevision:profiles.find(p=>p.id===profileId)?.revision});if(version===generation.current){await load();open(s.sessionId);}}catch(e){if(version===generation.current)setError(runtimeError(e));}finally{if(version===generation.current)setBusy(false);}}}>New runtime conversation</button>
    <label>Saved runtime conversation<select value="" disabled={busy} onChange={e=>{if(e.target.value)open(e.target.value);}}><option value="">Open in main view</option>{sessions.map(s=><option key={s.sessionId} value={s.sessionId}>{s.profile.name} · {s.sessionId}</option>)}</select></label>
    {error&&<p role="alert">{error}</p>}
  </section>;
}
