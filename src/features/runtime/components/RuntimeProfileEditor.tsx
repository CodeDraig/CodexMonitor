import {useState} from "react";
import {runtimeRequest} from "@services/tauri";
import {runtimeError,type AcpClientPolicy,type EnvironmentSource,type RuntimeProfile} from "../types";
import {AcpConnectionControls} from "./AcpConnectionControls";

type Preset = {id:string;name:string;executable:string;args:string[]};
const PRESETS:Preset[]=[
  {id:"opencode",name:"OpenCode",executable:"opencode",args:["acp"]},
  {id:"claude-acp",name:"Claude Agent ACP 0.94.0",executable:"claude-agent-acp",args:[]},
  {id:"gemini",name:"Gemini CLI 0.64.0",executable:"gemini",args:["--acp"]},
  {id:"antigravity-acp",name:"Google Antigravity ACP 1.4.1",executable:"./agy_acp_server.par",args:["--uid="]},
];
const DEFAULT_POLICY:AcpClientPolicy={filesystem:"disabled",commandExecution:"disabled",additionalRoots:[]};
function envText(environment:Record<string,EnvironmentSource>|undefined){return JSON.stringify(environment??{},null,2);}
function policyOf(profile:RuntimeProfile|undefined):AcpClientPolicy{return profile?.clientPolicy??DEFAULT_POLICY;}

export function RuntimeProfileEditor({profiles,onSaved,backendKey="local"}:{profiles:RuntimeProfile[];onSaved:()=>Promise<void>;backendKey?:string}){
  const [id,setId]=useState("opencode"),[name,setName]=useState("OpenCode"),[executable,setExecutable]=useState("opencode"),[args,setArgs]=useState('["acp"]'),[cwd,setCwd]=useState(""),[presetId,setPresetId]=useState<string|null>("opencode");
  const [environment,setEnvironment]=useState("{}"),[filesystem,setFilesystem]=useState<AcpClientPolicy["filesystem"]>("disabled"),[commandExecution,setCommandExecution]=useState<AcpClientPolicy["commandExecution"]>("disabled"),[roots,setRoots]=useState("");
  const [busy,setBusy]=useState(false),[error,setError]=useState<string|null>(null),[editingId,setEditingId]=useState<string|null>(null);
  const previous=profiles.find(p=>p.id===id);
  const selectPreset=(preset:Preset)=>{
    const known=profiles.find(p=>p.presetId===preset.id||p.id===preset.id);
    const legacyOpenCode=preset.id==="opencode"?profiles.find(p=>p.kind==="open_code_acp"):undefined;
    const p=known??legacyOpenCode;
    setEditingId(p?.id??null);setId(p?.id??preset.id);setName(p?.name??preset.name);
    setExecutable(p?.executable??preset.executable);
    setArgs(JSON.stringify(p?p.kind==="open_code_acp"?[...p.args,"acp"]:p.args:preset.args));
    setCwd(p?.workingDirectory??"");setPresetId(preset.id);
    setEnvironment(envText(p?.environment));const policy=policyOf(p);setFilesystem(policy.filesystem);setCommandExecution(policy.commandExecution);setRoots(policy.additionalRoots.join("\n"));
  };
  const selectExisting=(profileId:string)=>{
    const p=profiles.find(candidate=>candidate.id===profileId);
    if(!p){setEditingId(null);setId("");setName("");setExecutable("");setArgs("[]");setCwd("");setPresetId(null);setEnvironment("{}");setFilesystem("disabled");setCommandExecution("disabled");setRoots("");return;}
    setEditingId(p.id);setId(p.id);setName(p.name);setExecutable(p.executable);setArgs(JSON.stringify(p.kind==="open_code_acp"?[...p.args,"acp"]:p.args));setCwd(p.workingDirectory??"");setPresetId(p.presetId??null);setEnvironment(envText(p.environment));const policy=policyOf(p);setFilesystem(policy.filesystem);setCommandExecution(policy.commandExecution);setRoots(policy.additionalRoots.join("\n"));
  };
  return <details><summary>ACP profiles</summary>
    <label>Edit profile<select value={editingId??""} onChange={e=>selectExisting(e.target.value)}><option value="">New profile</option>{profiles.filter(p=>p.kind!=="codex").map(p=><option key={p.id} value={p.id}>{p.name}</option>)}</select></label>
    <fieldset><legend>Agent launch presets</legend>{PRESETS.map(preset=><button key={preset.id} className="ghost" type="button" onClick={()=>selectPreset(preset)}>{preset.name} preset</button>)}</fieldset>
    <form onSubmit={async e=>{
      e.preventDefault();setBusy(true);setError(null);
      try{
        if(previous&&editingId!==id)throw new Error("Select the existing profile before editing it");
        const parsedArgs:unknown=JSON.parse(args);if(!Array.isArray(parsedArgs)||!parsedArgs.every(v=>typeof v==="string"))throw new Error("Arguments must be a JSON array of strings");
        const parsedEnvironment:unknown=JSON.parse(environment);
        if(typeof parsedEnvironment!=="object"||parsedEnvironment===null||Array.isArray(parsedEnvironment))throw new Error("Environment must be a JSON object");
        for(const [key,source] of Object.entries(parsedEnvironment as Record<string,unknown>)){
          if(!/^[A-Za-z_][A-Za-z0-9_]*$/.test(key)||typeof source!=="object"||source===null||!("kind" in source)||!(["literal","backendEnv"] as unknown[]).includes((source as {kind:unknown}).kind))throw new Error(`Invalid environment source for ${key}`);
          if((source as {kind:string}).kind==="literal"&&typeof (source as {value?:unknown}).value!=="string")throw new Error(`Environment ${key} needs a literal value`);
          if((source as {kind:string}).kind==="backendEnv"&&typeof (source as {name?:unknown}).name!=="string")throw new Error(`Environment ${key} needs a backend variable name`);
        }
        const clientPolicy:AcpClientPolicy={filesystem,commandExecution,additionalRoots:roots.split(/\r?\n/).map(root=>root.trim()).filter(Boolean)};
        await runtimeRequest("save_profile",{expectedRevision:previous?.revision??0,profile:{id,revision:previous?.revision??0,name,kind:"acp",executable,args:parsedArgs,workingDirectory:cwd||null,presetId,environment:parsedEnvironment,clientPolicy}});
        await onSaved();setEditingId(id);
      }catch(e){setError(runtimeError(e));}finally{setBusy(false);}
    }}>
      <label>Profile ID<input value={id} onChange={e=>{setId(e.target.value);setEditingId(current=>current===e.target.value?current:null);}} required/></label>
      <label>Profile name<input value={name} onChange={e=>setName(e.target.value)} required/></label>
      <label>ACP executable<input value={executable} onChange={e=>setExecutable(e.target.value)} required/></label>
      <label>ACP arguments (JSON array)<input value={args} onChange={e=>setArgs(e.target.value)} required/></label>
      <label>Launch directory (optional)<input value={cwd} onChange={e=>setCwd(e.target.value)}/></label>
      <label>Environment sources (JSON; literal values or backend variable names)<textarea value={environment} onChange={e=>setEnvironment(e.target.value)} rows={4}/></label>
      <p>Secrets should use a variable name resolved on the selected backend. Monitor never reads or displays that variable's value. Install the version shown in the preset on the selected backend and make its executable available on that backend's PATH.</p>
      <label>Client filesystem callbacks<select value={filesystem} onChange={e=>setFilesystem(e.target.value as AcpClientPolicy["filesystem"])}><option value="disabled">Disabled</option><option value="read_only">Read only</option><option value="read_write">Read and write</option></select></label>
      <label>Client command execution<select value={commandExecution} onChange={e=>setCommandExecution(e.target.value as AcpClientPolicy["commandExecution"])}><option value="disabled">Disabled</option><option value="ask">Ask each time</option><option value="allow">Allow</option></select></label>
      {presetId==="gemini"&&(filesystem==="disabled"||commandExecution==="disabled")&&<p role="note">Gemini CLI 0.64 requires filesystem and terminal callbacks for normal ACP operation. Set filesystem callbacks to Ask/Allow as appropriate, and command execution to Ask/Allow.</p>}
      <label>Additional filesystem roots (one backend path per line)<textarea value={roots} onChange={e=>setRoots(e.target.value)} rows={3}/></label>
      <p>ACP callbacks run on the selected backend. Commands use that backend account's permissions.</p>
      <button className="ghost" disabled={busy||!id.trim()}>Save ACP profile</button>
    </form>
    {error&&<p role="alert">{error}</p>}
    {editingId&&profiles.some(p=>p.id===editingId)&&<AcpConnectionControls backendKey={backendKey} profile={profiles.find(p=>p.id===editingId)!}/>}
  </details>;
}
