// @vitest-environment jsdom
import {cleanup,fireEvent,render,screen,waitFor} from "@testing-library/react";
import {afterEach,expect,it,vi} from "vitest";
import {RuntimePanel} from "./RuntimePanel";
import {runtimeRequest} from "@services/tauri";
vi.mock("@services/tauri",()=>({runtimeRequest:vi.fn()}));
afterEach(()=>{cleanup();vi.resetAllMocks();});
it("opens existing and new runtime sessions in the normal main conversation without a home viewer",async()=>{
 const profile={id:"p",revision:1,name:"Generic",kind:"acp",executable:"agent",args:[]};
 let finish:(value:unknown)=>void=()=>{};const opened:unknown[]=[];
 const listener=(e:Event)=>opened.push((e as CustomEvent).detail);window.addEventListener("runtime-open-conversation",listener);
 vi.mocked(runtimeRequest).mockImplementation(async(method)=>{
  if(method==="profiles")return {storeId:"store",profiles:[profile]};
  if(method==="sessions")return [{sessionId:"old",profile}];
  if(method==="create_session")return new Promise(resolve=>{finish=resolve;});
  throw new Error(method);
 });
 render(<RuntimePanel workspaceId="workspace" backendKey="local" requestedSessionId="old"/>);
 await waitFor(()=>expect(opened).toContainEqual({backendKey:"local",sessionId:"old"}));
 await waitFor(()=>expect((screen.getByRole("button",{name:"New runtime conversation"}) as HTMLButtonElement).disabled).toBe(false));
 fireEvent.change(screen.getByLabelText("Conversation agent"),{target:{value:"p"}});
 fireEvent.click(screen.getByRole("button",{name:"New runtime conversation"}));
 expect(screen.queryByLabelText("Runtime message")).toBeNull();finish({sessionId:"new",profile});
 await waitFor(()=>expect(opened).toContainEqual({backendKey:"local",sessionId:"new"}));
 expect(vi.mocked(runtimeRequest).mock.calls.some(([method])=>method==="load_session")).toBe(false);
 window.removeEventListener("runtime-open-conversation",listener);
});
