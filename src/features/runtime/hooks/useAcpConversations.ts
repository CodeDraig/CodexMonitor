import {useCallback,useEffect,useMemo,useRef,useState} from "react";
import type {ThreadSummary,WorkspaceInfo} from "@/types";
import {runtimeRequest} from "@services/tauri";
import {subscribeRuntimeEvents} from "@services/events";
import {acpRef,conversationKey} from "../conversationIdentity";
import {conversationItems} from "../conversationProjection";
import {projectRuntimeEvents} from "../projection";
import {useRuntimeConversation} from "./useRuntimeConversation";
import {runtimeError,type CatalogSession,type ConversationPage,type ExecutionTicket,type RuntimeProfile,type RuntimeSession} from "../types";

type Entry={requestId:string;text:string;images:string[];content:Record<string,unknown>[]};
type Options={backendKey:string;workspaces:WorkspaceInfo[];activeThreadId:string|null;onOpen:(workspaceId:string,key:string)=>void;refreshWorkspaces:()=>Promise<unknown>};
export function useAcpConversations({backendKey,workspaces,activeThreadId,onOpen,refreshWorkspaces}:Options){
  const [catalog,setCatalog]=useState<CatalogSession[]>([]);
  const [profiles,setProfiles]=useState<RuntimeProfile[]>([]);
  const [statusById,setStatusById]=useState<Record<string,{isProcessing:boolean;isReviewing:boolean;hasUnread:boolean;processingStartedAt:number|null;lastDurationMs:number|null}>>({});
  const [sourceErrors,setSourceErrors]=useState<Record<string,string>>({});
  const [browserOpen,setBrowserOpen]=useState(false);
  const [session,setSession]=useState<RuntimeSession|null>(null);
  const [page,setPage]=useState<ConversationPage|null>(null);
  const [rows,setRows]=useState<Record<string,unknown>[]>([]);
  const [error,setError]=useState<string|null>(null);
  const [loading,setLoading]=useState(false);
  const [sending,setSending]=useState(false);
  const [requests,setRequests]=useState<Record<string,{pending:string|null;uncertain:string|null}>>({});
  const [queues,setQueues]=useState<Record<string,Entry[]>>({});
  const [drafts,setDrafts]=useState<Record<string,string>>({});
  const [contentDrafts,setContentDrafts]=useState<Record<string,Record<string,unknown>[]>>({});
  const [imageDrafts,setImageDrafts]=useState<Record<string,string[]>>({});
  const ref=acpRef(activeThreadId);
  const selected=Boolean(ref);
  const key=ref&&ref.backendId===backendKey?activeThreadId:null;
  const pending=key?requests[key]?.pending??null:null,uncertain=key?requests[key]?.uncertain??null:null;
  const setPending=(pending:string|null)=>{if(key)setRequests(p=>({...p,[key]:{pending,uncertain:p[key]?.uncertain??null}}));};
  const setUncertain=(uncertain:string|null)=>{if(key)setRequests(p=>({...p,[key]:{pending:p[key]?.pending??null,uncertain}}));};
  const epoch=useRef(0),catalogEpoch=useRef(0),writing=useRef(false);
  const currentKey=useRef(key);currentKey.current=key;
  useEffect(()=>subscribeRuntimeEvents(event=>{
    if(event.source!=="live"||event.runtime?.kind==="codex")return;
    const id=conversationKey({kind:"acp",backendId:backendKey,storeId:event.storeId,sessionId:event.sessionId});
    const terminal=event.kind==="execution_completed"||event.kind==="disconnected";
    const activity=event.kind==="message_delta"||event.kind==="tool_patch";
    const processing=event.kind==="execution_submitted"||activity&&!!event.executionId;
    if(!terminal&&!activity&&!processing)return;
    setStatusById(p=>{const old=p[id];return {...p,[id]:{isProcessing:terminal?false:processing||old?.isProcessing||false,isReviewing:false,hasUnread:currentKey.current===id?false:activity||old?.hasUnread||false,processingStartedAt:processing?old?.processingStartedAt??Date.now():null,lastDurationMs:terminal&&old?.processingStartedAt?Date.now()-old.processingStartedAt:old?.lastDurationMs??null}};});
  }),[backendKey]);
  useEffect(()=>{if(key)setStatusById(p=>p[key]?.hasUnread?{...p,[key]:{...p[key],hasUnread:false}}:p);},[key]);
  const workspaceRef=useRef(workspaces);workspaceRef.current=workspaces;
  const callbacks=useRef({onOpen,refreshWorkspaces});callbacks.current={onOpen,refreshWorkspaces};
  const {events,executions,error:journalError,reload}=useRuntimeConversation(session,backendKey);
  const projected=useMemo(()=>projectRuntimeEvents(events),[events]);
  const active=executions.find(t=>["not_started","running","unknown"].includes(t.execution));
  const queue=key?queues[key]??[]:[];
  const hasPermissions=Object.keys(projected.permissions).length>0;
  const busy=Boolean(active||pending||sending||uncertain||queue.length||hasPermissions);
  const busyRef=useRef(busy);busyRef.current=busy;
  const refreshCatalog=useCallback(async()=>{
    const version=catalogEpoch.current;
    const [info,known]=await Promise.all([runtimeRequest<{profiles:RuntimeProfile[]}>("profiles"),runtimeRequest<CatalogSession[]>("catalog")]);
    if(version!==catalogEpoch.current)return;
    // Attaching a known directory only registers identity; it does not start a runtime.
    for(const row of known){
      if(row.sessionId||row.profile.kind==="codex")continue;
      const workspace=workspaceRef.current.find(w=>w.id===row.workspaceId||w.path===row.cwd);
      if(workspace){try{const attached=await runtimeRequest<RuntimeSession>("attach_session",{catalogId:row.catalogId,workspaceAssociation:{kind:"reuse",workspaceId:workspace.id}});if(version!==catalogEpoch.current)return;row.sessionId=attached.sessionId;}catch{/* Keep unavailable directories in the global browser. */}}
    }
    if(version===catalogEpoch.current){setProfiles(info.profiles);setCatalog(known);}
  },[]);
  useEffect(()=>{
    ++catalogEpoch.current;setCatalog([]);setProfiles([]);
    const refresh=()=>{void refreshCatalog().catch(e=>setError(runtimeError(e)));};refresh();
    const timer=window.setInterval(refresh,15000);
    return()=>{++catalogEpoch.current;window.clearInterval(timer);};
  },[backendKey,refreshCatalog]);
  const discoverAll=useCallback(async()=>{
    const version=catalogEpoch.current;
    const info=await runtimeRequest<{profiles:RuntimeProfile[]}>("profiles");
    const agents=info.profiles.filter(p=>p.kind!=="codex");
    const results=await Promise.allSettled(agents.map(p=>runtimeRequest("discover_sessions",{profileId:p.id})));
    if(version===catalogEpoch.current)setSourceErrors(Object.fromEntries(results.flatMap((r,i)=>r.status==="rejected"?[[agents[i].id,runtimeError(r.reason)]]:[])));
    if(version===catalogEpoch.current)await refreshCatalog();
  },[refreshCatalog]);
  useEffect(()=>{const discover=()=>{void discoverAll().catch(()=>{});};discover();const timer=window.setInterval(discover,30000);window.addEventListener("focus",discover);return()=>{window.clearInterval(timer);window.removeEventListener("focus",discover);};},[backendKey,discoverAll]);
  const readRef=useRef<()=>Promise<void>>(async()=>{});
  const refreshRef=useRef<()=>Promise<void>>(async()=>{});
  useEffect(()=>{
    const version=++epoch.current;
    setSession(null);setPage(null);setRows([]);setError(null);setLoading(Boolean(key));writing.current=false;setSending(false);
    if(!key||!ref)return;
    let reading=false,replaying=false;
    let latest:ConversationPage|null=null;
    let loadSupported:boolean|undefined;
    const valid=()=>version===epoch.current;
    const read=async()=>{
      if(reading||!valid())return;reading=true;
      try{
        let cursor:number|null=null,generation:string|undefined;const items:Record<string,unknown>[]=[];let result:ConversationPage;
        do{result=await runtimeRequest<ConversationPage>("read_conversation",{sessionId:ref.sessionId,cursor,generation,limit:200});
          if(!valid())return;if(result.storeId!==ref.storeId)throw new Error("Runtime store changed; reopen the conversation");
          generation=result.generation;items.push(...result.items);cursor=result.nextCursor;
        }while(cursor!==null);
        if(valid()){latest=result;setPage(result);setRows(items);setLoading(result.load?.state==="loading");
          const saved=await runtimeRequest<RuntimeSession>("session",{sessionId:ref.sessionId});if(valid()){loadSupported=saved.agentInfo&&Object.keys(saved.agentInfo).length?saved.capabilities.loadSession:undefined;setSession(previous=>JSON.stringify(previous)===JSON.stringify(saved)?previous:saved);}
        }
      }catch(e){if(valid())setError(runtimeError(e));}finally{reading=false;}
    };
    const replay=async(force=false)=>{
      // A replay disables configuration controls. Keep native dropdowns open
      // while the user is choosing a model, mode, or other session option.
      if(!force&&document.activeElement?.closest(".acp-conversation-controls"))return;
      if(loadSupported===false||!valid()||replaying||busyRef.current||latest?.readOnly||document.visibilityState==="hidden")return;
      if(latest?.load?.state==="loading")return;
      replaying=true;
      try{await runtimeRequest("load_conversation_history",{sessionId:ref.sessionId,requestId:crypto.randomUUID()});if(valid()){setError(null);setLoading(true);await read();}}
      catch(e){if(valid())setError(runtimeError(e));}finally{replaying=false;}
    };
    readRef.current=read;refreshRef.current=()=>replay(true);
    const start=async()=>{await read();if(valid())await replay();};void start();
    const unsubscribe=subscribeRuntimeEvents(e=>{if(e.storeId===ref.storeId&&e.sessionId===ref.sessionId){void read();if(e.kind==="session_info_update")void replay();}});
    const poll=window.setInterval(()=>{void read();},1500);
    const fresh=window.setInterval(()=>{void replay();},30000);
    const focus=()=>{void replay();};window.addEventListener("focus",focus);document.addEventListener("visibilitychange",focus);
    return()=>{++epoch.current;unsubscribe();window.clearInterval(poll);window.clearInterval(fresh);window.removeEventListener("focus",focus);document.removeEventListener("visibilitychange",focus);};
  // Identity owns all requests, cursors, queues and permission epochs.
  },[key]);
  useEffect(()=>{if(pending&&executions.some(t=>t.executionId===pending&&["completed","failed","interrupted"].includes(t.execution)))setPending(null);},[pending,executions]);
  const dispatch=useCallback(async(entry:Entry)=>{
    const selectedKey=currentKey.current;if(!selectedKey||!session||writing.current)return;
    writing.current=true;setSending(true);setError(null);
    try{
      const prompt:Record<string,unknown>[]=[{type:"text",text:entry.text},...entry.content];
      for(const image of entry.images){const match=/^data:([^;,]+);base64,(.*)$/s.exec(image);if(!match)throw new Error("ACP image attachments require a data URL");prompt.push({type:"image",mimeType:match[1],data:match[2]});}
      const receipt=await runtimeRequest<ExecutionTicket>("submit",{requestId:entry.requestId,sessionId:session.sessionId,prompt,options:{}});
      setRequests(p=>({...p,[selectedKey]:{pending:receipt.executionId,uncertain:null}}));
      if(currentKey.current===selectedKey){reload();await readRef.current();}
    }catch(e){setRequests(p=>({...p,[selectedKey]:{pending:null,uncertain:entry.requestId}}));if(currentKey.current===selectedKey)setError(runtimeError(e));}
    finally{if(currentKey.current===selectedKey){writing.current=false;setSending(false);}}
  },[session,reload]);
  useEffect(()=>{
    if(key&&session&&!active&&!pending&&!sending&&!uncertain&&!hasPermissions&&!loading&&!page?.readOnly&&queue.length){
      const [next,...rest]=queue;setQueues(previous=>({...previous,[key]:rest}));void dispatch(next);
    }
  },[key,session,active,pending,sending,uncertain,hasPermissions,loading,page?.readOnly,queue,dispatch]);
  const send=(text:string,images:string[])=>{
    if(!key||!session||page?.readOnly||uncertain)return;
    const entry={requestId:crypto.randomUUID(),text,images,content:contentDrafts[key]??[]};setContentDrafts(p=>({...p,[key]:[]}));setDrafts(p=>({...p,[key]:""}));setImageDrafts(p=>({...p,[key]:[]}));
    if(busy||loading)setQueues(p=>({...p,[key]:[...(p[key]??[]),entry]}));else void dispatch(entry);
  };
  const open=useCallback(async(row:CatalogSession,add=false)=>{
    const version=catalogEpoch.current;
    const workspace=workspaceRef.current.find(w=>w.id===row.workspaceId||w.path===row.cwd);
    if(!workspace&&!add)throw new Error("Add this session's workspace before opening it");
    const s=await runtimeRequest<RuntimeSession>("attach_session",{catalogId:row.catalogId,workspaceAssociation:workspace?{kind:"reuse",workspaceId:workspace.id}:{kind:"add"}});
    await callbacks.current.refreshWorkspaces();
    if(version!==catalogEpoch.current)return;
    await refreshCatalog();if(version!==catalogEpoch.current)return;callbacks.current.onOpen(s.workspaceId,s.runtime.kind==="codex"?s.nativeSessionId:conversationKey({kind:"acp",backendId:backendKey,storeId:s.storeId,sessionId:s.sessionId}));setBrowserOpen(false);
  },[backendKey,refreshCatalog]);
  useEffect(()=>{
    const listener=(e:Event)=>{const detail=(e as CustomEvent<{backendKey:string;sessionId:string}>).detail;if(detail.backendKey!==backendKey)return;
      const version=catalogEpoch.current;void runtimeRequest<RuntimeSession>("session",{sessionId:detail.sessionId}).then(async s=>{if(version!==catalogEpoch.current)return;if(!workspaceRef.current.some(w=>w.id===s.workspaceId)){setBrowserOpen(true);setError("Add this conversation’s workspace in Browse agent sessions before opening it");return;}await refreshCatalog();if(version!==catalogEpoch.current)return;callbacks.current.onOpen(s.workspaceId,s.runtime.kind==="codex"?s.nativeSessionId:conversationKey({kind:"acp",backendId:backendKey,storeId:s.storeId,sessionId:s.sessionId}));}).catch(e=>setError(runtimeError(e)));};
    window.addEventListener("runtime-open-conversation",listener);return()=>window.removeEventListener("runtime-open-conversation",listener);
  },[backendKey,refreshCatalog]);
  const metadata=useCallback(async(id:string,patch:Record<string,unknown>)=>{
    const version=catalogEpoch.current;
    const target=acpRef(id);const row=catalog.find(r=>r.sessionId===target?.sessionId&&r.storeId===target.storeId);
    if(!row||target?.backendId!==backendKey)throw new Error("Conversation metadata is not available on this backend");
    const updated=await runtimeRequest<CatalogSession>("update_conversation_metadata",{sessionId:row.sessionId,expectedRevision:row.revision,...patch});
    if(version===catalogEpoch.current)setCatalog(rows=>rows.map(r=>r.catalogId===updated.catalogId?updated:r));
  },[catalog,backendKey]);
  const summaries=useMemo(()=>{
    const result:Record<string,ThreadSummary[]>={};
    for(const row of catalog){if(!row.sessionId||row.presentation.hidden)continue;
      const workspace=workspaces.find(w=>w.id===row.workspaceId||w.path===row.cwd);if(!workspace)continue;
      const conversationRef=row.profile.kind==="codex"?{kind:"codex" as const,threadId:row.nativeSessionId}:{kind:"acp" as const,backendId:backendKey,storeId:row.storeId,sessionId:row.sessionId};
      (result[workspace.id]??=[]).push({id:conversationKey(conversationRef),conversationRef,agentLabel:row.profile.name,name:row.presentation.titleOverride??row.title??row.nativeSessionId,updatedAt:Date.parse(row.updatedAt??"")||0});
    }return result;
  },[catalog,workspaces,backendKey]);
  return {hasWorkspace:(cwd:string,workspaceId?:string|null)=>workspaces.some(w=>w.id===workspaceId||w.path===cwd),backendKey,catalog,profiles,statusById,sourceErrors,browserOpen,setBrowserOpen,refreshCatalog,discoverAll,open,metadata,summaries,selected,session:key?session:null,page:key?page:null,loading,items:key?conversationItems(rows,session?.sessionId??""):[],permissions:projected.permissions,
    error:error??journalError,sending,active,pending,uncertain,queue,draft:key?drafts[key]??"":"",images:key?imageDrafts[key]??[]:[],
    setDraft:(text:string)=>{if(key)setDrafts(p=>({...p,[key]:text}));},setImages:(images:string[])=>{if(key)setImageDrafts(p=>({...p,[key]:images}));},send,
    newConversation:async(workspaceId:string,profileId:string)=>{const version=catalogEpoch.current;const profile=profiles.find(p=>p.id===profileId);const s=await runtimeRequest<RuntimeSession>("create_session",{workspaceId,profileId,expectedProfileRevision:profile?.revision});if(version===catalogEpoch.current){await refreshCatalog();if(version!==catalogEpoch.current)return;callbacks.current.onOpen(s.workspaceId,s.runtime.kind==="codex"?s.nativeSessionId:conversationKey({kind:"acp",backendId:backendKey,storeId:s.storeId,sessionId:s.sessionId}));}},
    removeQueued:(requestId:string)=>{if(key)setQueues(p=>({...p,[key]:(p[key]??[]).filter(e=>e.requestId!==requestId)}));},
    content:key?contentDrafts[key]??[]:[],
    addContent:(content:Record<string,unknown>)=>{if(key)setContentDrafts(p=>({...p,[key]:[...(p[key]??[]),content]}));},
    clearContent:()=>{if(key)setContentDrafts(p=>({...p,[key]:[]}));},
    commands:(rows.filter(r=>r.type==="available_commands_update").slice(-1)[0]?.data as {availableCommands?:{name:string;description?:string}[]}|undefined)?.availableCommands??[],
    refresh:async(targetKey=key)=>{if(targetKey===key)return refreshRef.current();const target=acpRef(targetKey);if(!target||target.backendId!==backendKey||!catalog.some(r=>r.sessionId===target.sessionId&&r.storeId===target.storeId))throw new Error("Conversation is not available on this backend");await runtimeRequest("load_conversation_history",{sessionId:target.sessionId,requestId:crypto.randomUUID()});},
    stop:async()=>{if(!session)return;if(key)setQueues(p=>({...p,[key]:[]}));try{await runtimeRequest("cancel",{sessionId:session.sessionId});reload();}catch(e){setError(runtimeError(e));}},
    permission:async(requestId:string,optionId:string)=>{if(!session)return;try{await runtimeRequest("permission",{sessionId:session.sessionId,requestId,optionId});reload();}catch(e){setError(runtimeError(e));}},
    configure:async(optionId:string,value:string)=>{if(!session)return;try{await runtimeRequest("configure",{sessionId:session.sessionId,optionId,value});await readRef.current();}catch(e){setError(runtimeError(e));}},
    checkReceipt:async()=>{if(!uncertain)return;try{const receipt=await runtimeRequest<ExecutionTicket|null>("execution",{requestId:uncertain});if(receipt){setUncertain(null);setPending(receipt.executionId);setError(null);reload();}else setError("No durable receipt found for "+uncertain);}catch(e){setError(runtimeError(e));}}
  };
}
export type AcpConversations=ReturnType<typeof useAcpConversations>;
