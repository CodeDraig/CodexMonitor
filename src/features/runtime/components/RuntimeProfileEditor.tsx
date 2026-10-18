import {useState} from "react";
import {runtimeRequest} from "@services/tauri";
import {runtimeError,type RuntimeProfile} from "../types";
export function RuntimeProfileEditor({profiles,onSaved}:{profiles:RuntimeProfile[];onSaved:()=>Promise<void>}){
  const [id,setId]=useState("opencode"),[name,setName]=useState("OpenCode"),[executable,setExecutable]=useState("opencode"),[args,setArgs]=useState('["acp"]'),[cwd,setCwd]=useState("");
  const [busy,setBusy]=useState(false),[error,setError]=useState<string|null>(null);
  const [editingId,setEditingId]=useState<string|null>(null);
  const previous=profiles.find(p=>p.id===id);
  return <details><summary>ACP profiles</summary><label>Edit profile<select value={editingId??""} onChange={e=>{const p=profiles.find(p=>p.id===e.target.value);if(p){setEditingId(p.id);setId(p.id);setName(p.name);setExecutable(p.executable);setArgs(JSON.stringify(p.kind==="open_code_acp"?[...p.args,"acp"]:p.args));setCwd(p.workingDirectory??"");}else{setEditingId(null);setId("");setName("");setExecutable("");setArgs("[]");setCwd("");}}}><option value="">New profile</option>{profiles.filter(p=>p.kind!=="codex").map(p=><option key={p.id} value={p.id}>{p.name}</option>)}</select></label>
    <button className="ghost" type="button" onClick={()=>{setEditingId(profiles.some(p=>p.id==="opencode")?"opencode":null);setId("opencode");setName("OpenCode");setExecutable("opencode");setArgs('["acp"]');setCwd("");}}>OpenCode launch preset</button>
    <form onSubmit={async e=>{e.preventDefault();setBusy(true);setError(null);try{if(previous&&editingId!==id)throw new Error("Select the existing profile before editing it");const parsed:unknown=JSON.parse(args);if(!Array.isArray(parsed)||!parsed.every(v=>typeof v==="string"))throw new Error("Arguments must be a JSON array of strings");await runtimeRequest("save_profile",{expectedRevision:previous?.revision??0,profile:{id,revision:previous?.revision??0,name,kind:"acp",executable,args:parsed,workingDirectory:cwd||null}});await onSaved();}catch(e){setError(runtimeError(e));}finally{setBusy(false);}}}>
      <label>Profile ID<input value={id} onChange={e=>setId(e.target.value)} required/></label><label>Profile name<input value={name} onChange={e=>setName(e.target.value)} required/></label><label>ACP executable<input value={executable} onChange={e=>setExecutable(e.target.value)} required/></label><label>ACP arguments<input value={args} onChange={e=>setArgs(e.target.value)} required/></label><label>Launch directory (optional)<input value={cwd} onChange={e=>setCwd(e.target.value)}/></label><p>The executable receives exactly these arguments. Session directories are supplied through ACP.</p><button className="ghost" disabled={busy}>Save ACP profile</button>
    </form>{error&&<p role="alert">{error}</p>}
  </details>;
}
