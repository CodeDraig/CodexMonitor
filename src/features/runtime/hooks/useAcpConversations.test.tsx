// @vitest-environment jsdom
import {act,cleanup,renderHook,waitFor} from "@testing-library/react";
import {afterEach,beforeEach,expect,it,vi} from "vitest";
import {runtimeRequest} from "@services/tauri";
import {conversationKey} from "../conversationIdentity";
import {useAcpConversations} from "./useAcpConversations";
import type {ExecutionTicket,RuntimeSession} from "../types";
vi.mock("@services/tauri",()=>({runtimeRequest:vi.fn()}));
vi.mock("@services/events",()=>({subscribeRuntimeEvents:vi.fn(()=>()=>{}),subscribeBackendResync:vi.fn(()=>()=>{})}));
const journal=vi.hoisted(()=>({executions:[] as ExecutionTicket[],events:[],error:null,reload:vi.fn()}));
vi.mock("./useRuntimeConversation",()=>({useRuntimeConversation:()=>journal}));
const session:RuntimeSession={storeId:"store",runtime:{profileId:"generic",profileRevision:1,kind:"acp"},sessionId:"session",nativeSessionId:"native-id",workspaceId:"w",cwd:"/remote/repo",profile:{id:"generic",revision:1,name:"Independent",kind:"acp",executable:"agent",args:["--protocol"]},capabilities:{loadSession:true,sessionDiscovery:true,images:true,configuration:false,cancellation:true,steering:false,permissions:true},configuration:{},agentInfo:{agentInfo:{name:"Independent"}}};
const workspace={id:"w",name:"Workspace",path:"/remote/repo",connected:false,settings:{sidebarCollapsed:false,codexAutoConnect:false}};
const key=conversationKey({kind:"acp",backendId:"backend",storeId:"store",sessionId:"session"});
let generation="generation1";
beforeEach(()=>{journal.executions=[];vi.resetAllMocks();vi.mocked(runtimeRequest).mockImplementation(async(method,input)=>{
 if(method==="profiles")return {storeId:"store",profiles:[session.profile]};
 if(method==="catalog")return [{catalogId:"catalog",storeId:"store",connectionScope:"scope",nativeSessionId:session.nativeSessionId,sessionId:session.sessionId,profile:session.profile,cwd:session.cwd,title:"External",updatedAt:null,metadata:{},revision:0,presentation:{}}];
 if(method==="discover_sessions")return {sessions:[],capabilities:session.capabilities,nextCursor:null,sourceStatus:"ready"};
 if(method==="session")return session;
 if(method==="read_conversation")return {storeId:"store",sessionId:"session",generation,items:[{id:"u",kind:"message",role:"user",text:"first"},{id:"a",kind:"message",role:"assistant",text:generation}],nextCursor:null,load:{id:generation,state:"complete",error:null},coverage:"agent_replay",freshness:{loadedAt:Date.now(),stale:false},readOnly:false,activeExecutionId:null};
 if(method==="load_conversation_history")return {id:"ticket",requestId:input?.requestId,state:"loading"};
 if(method==="submit")return {executionId:input?.requestId,requestId:input?.requestId,execution:"running",submission:"accepted"};
 throw new Error(`Unexpected Codex or runtime operation: ${method}`);
 });});
afterEach(()=>{cleanup();vi.restoreAllMocks();vi.useRealTimers();});
it("loads the normal conversation, preserves a draft through idle refresh, and makes no Codex calls",async()=>{
 const timers:(()=>void|Promise<void>)[]=[];const interval=window.setInterval.bind(window);
 vi.spyOn(window,"setInterval").mockImplementation((fn,ms,...args)=>{if(ms===30000&&typeof fn==="function")timers.push(fn as ()=>void);return interval(fn,ms,...args);});
 const {result}=renderHook(()=>useAcpConversations({backendKey:"backend",workspaces:[workspace],activeThreadId:key,onOpen:vi.fn(),refreshWorkspaces:vi.fn()}));
 await waitFor(()=>expect(result.current.items).toHaveLength(2));
 await act(async()=>{result.current.setDraft("keep this draft");});generation="generation2";
 await act(async()=>{for(const timer of timers)await timer();});
 await waitFor(()=>expect(result.current.items[1]).toMatchObject({text:"generation2"}));expect(result.current.draft).toBe("keep this draft");
 expect(runtimeRequest).toHaveBeenCalledWith("load_conversation_history",expect.objectContaining({sessionId:"session"}));
 expect(vi.mocked(runtimeRequest).mock.calls.every(([m])=>["profiles","catalog","discover_sessions","session","read_conversation","load_conversation_history"].includes(m))).toBe(true);
 expect(result.current.summaries.w[0].conversationRef).toEqual({kind:"acp",backendId:"backend",storeId:"store",sessionId:"session"});
});
it("queues a follow-up once, submits it only after completion, and scopes caches across backend switches",async()=>{
 const onOpen=vi.fn(),refreshWorkspaces=vi.fn();
 const {result,rerender}=renderHook(({backendKey})=>useAcpConversations({backendKey,workspaces:[workspace],activeThreadId:key,onOpen,refreshWorkspaces}),{initialProps:{backendKey:"backend"}});
 await waitFor(()=>expect(result.current.session?.sessionId).toBe("session"));
 await act(async()=>{result.current.send("first prompt",[]);});
 await waitFor(()=>expect(result.current.pending).toBeTruthy());
 const first=result.current.pending!;
 await act(async()=>{result.current.send("queued prompt",[]);});expect(result.current.queue).toHaveLength(1);
 expect(vi.mocked(runtimeRequest).mock.calls.filter(([m])=>m==="submit")).toHaveLength(1);
 journal.executions=[{executionId:first,execution:"completed",submission:"accepted"} as ExecutionTicket];
 await act(async()=>{rerender({backendKey:"backend"});});
 await waitFor(()=>expect(vi.mocked(runtimeRequest).mock.calls.filter(([m])=>m==="submit")).toHaveLength(2));expect(result.current.queue).toHaveLength(0);
 await act(async()=>{rerender({backendKey:"other-backend"});});expect(result.current.session).toBeNull();expect(result.current.items).toHaveLength(0);
});
it("opens the same recorded conversation repeatedly through the matching backend and ignores foreign links",async()=>{
 const onOpen=vi.fn();
 renderHook(()=>useAcpConversations({backendKey:"backend",workspaces:[workspace],activeThreadId:null,onOpen,refreshWorkspaces:vi.fn()}));
 await act(async()=>{window.dispatchEvent(new CustomEvent("runtime-open-conversation",{detail:{backendKey:"other",sessionId:"session"}}));});
 expect(onOpen).not.toHaveBeenCalled();
 for(let count=1;count<=2;count++){
  await act(async()=>{window.dispatchEvent(new CustomEvent("runtime-open-conversation",{detail:{backendKey:"backend",sessionId:"session"}}));});
  await waitFor(()=>expect(onOpen).toHaveBeenCalledTimes(count));
 }
 expect(onOpen).toHaveBeenLastCalledWith("w",key);
});
it("refreshes the requested sidebar conversation when it is not selected and rejects foreign identities",async()=>{
 const {result}=renderHook(()=>useAcpConversations({backendKey:"backend",workspaces:[workspace],activeThreadId:null,onOpen:vi.fn(),refreshWorkspaces:vi.fn()}));
 await waitFor(()=>expect(result.current.catalog).toHaveLength(1));
 await act(async()=>{await result.current.refresh(key);});
 expect(runtimeRequest).toHaveBeenCalledWith("load_conversation_history",expect.objectContaining({sessionId:"session"}));
 const foreign=conversationKey({kind:"acp",backendId:"other",storeId:"store",sessionId:"session"});
 await expect(result.current.refresh(foreign)).rejects.toThrow("not available on this backend");
});
it("defers idle and focus replay while an ACP selector is focused, then resumes after leaving the controls",async()=>{
 const timers:(()=>void|Promise<void>)[]=[];const interval=window.setInterval.bind(window);
 vi.spyOn(window,"setInterval").mockImplementation((fn,ms,...args)=>{if(ms===30000&&typeof fn==="function")timers.push(fn as ()=>void);return interval(fn,ms,...args);});
 const {result}=renderHook(()=>useAcpConversations({backendKey:"backend",workspaces:[workspace],activeThreadId:key,onOpen:vi.fn(),refreshWorkspaces:vi.fn()}));
 await waitFor(()=>expect(result.current.session?.sessionId).toBe("session"));
 await waitFor(()=>expect(vi.mocked(runtimeRequest).mock.calls.some(([m])=>m==="load_conversation_history")).toBe(true));
 const controls=document.createElement("div");controls.className="acp-conversation-controls";
 const select=document.createElement("select");controls.append(select);document.body.append(controls);
 const replayCalls=()=>vi.mocked(runtimeRequest).mock.calls.filter(([m])=>m==="load_conversation_history").length;
 try{
  select.focus();const before=replayCalls();
  await act(async()=>{window.dispatchEvent(new Event("focus"));for(const timer of timers)await timer();});
  expect(replayCalls()).toBe(before);
  await act(async()=>{await result.current.refresh();});
  expect(replayCalls()).toBeGreaterThan(before);
  const afterManual=replayCalls();
  select.blur();
  await act(async()=>{for(const timer of timers)await timer();});
  expect(replayCalls()).toBeGreaterThan(afterManual);
 }finally{controls.remove();}
});
