// @vitest-environment jsdom
import {cleanup,fireEvent,render,screen,waitFor} from "@testing-library/react";
import {afterEach,beforeEach,expect,it,vi} from "vitest";
import {RuntimeConversation} from "./RuntimeConversation";
import {runtimeRequest} from "@services/tauri";
import {useRuntimeConversation} from "../hooks/useRuntimeConversation";
import type {RuntimeSession} from "../types";
vi.mock("@services/tauri",()=>({runtimeRequest:vi.fn()}));
vi.mock("../hooks/useRuntimeConversation",()=>({useRuntimeConversation:vi.fn()}));
vi.mock("@/features/messages/components/MessageRow",()=>({MessageRow:()=>null}));
vi.mock("@/features/messages/components/ToolRow",()=>({ToolRow:()=>null}));
const session:RuntimeSession={storeId:"s",runtime:{profileId:"opencode",profileRevision:1,kind:"open_code_acp"},sessionId:"session",nativeSessionId:"native",workspaceId:"w",cwd:"/tmp",profile:{id:"opencode",revision:1,name:"OpenCode",kind:"open_code_acp",executable:"opencode",args:[]},capabilities:{loadSession:true,sessionDiscovery:false,images:false,configuration:false,cancellation:true,steering:false,permissions:true},configuration:{},agentInfo:{}};
afterEach(cleanup);
beforeEach(()=>{vi.clearAllMocks();vi.mocked(runtimeRequest).mockResolvedValue({executionId:"second"});vi.mocked(useRuntimeConversation).mockReturnValue({events:[],executions:[],error:null,reload:vi.fn()});});
it("queues once until the active execution ends and never calls a Codex command",async()=>{
 const active={executionId:"first",requestId:"request",sessionId:"session",submission:"accepted" as const,execution:"running" as const,nativeTurnId:null,input:{requestId:"request",sessionId:"session",prompt:[]},error:null};
 vi.mocked(useRuntimeConversation).mockReturnValue({events:[],executions:[active],error:null,reload:vi.fn()});
 const view=render(<RuntimeConversation session={session} backendKey="local" onConfiguration={vi.fn()}/>);
 fireEvent.change(screen.getByLabelText("Runtime message"),{target:{value:"queued prompt"}});fireEvent.click(screen.getByRole("button",{name:"Queue message"}));
 expect(runtimeRequest).not.toHaveBeenCalled();
 vi.mocked(useRuntimeConversation).mockReturnValue({events:[],executions:[{...active,execution:"completed"}],error:null,reload:vi.fn()});
 view.rerender(<RuntimeConversation session={session} backendKey="local" onConfiguration={vi.fn()}/>);
 await waitFor(()=>expect(runtimeRequest).toHaveBeenCalledTimes(1));expect(runtimeRequest).toHaveBeenCalledWith("submit",expect.objectContaining({sessionId:"session",prompt:[{type:"text",text:"queued prompt"}]}));
 view.rerender(<RuntimeConversation session={session} backendKey="local" onConfiguration={vi.fn()}/>);expect(runtimeRequest).toHaveBeenCalledTimes(1);
});
it("keeps a failed submission request for lookup rather than sending it again",async()=>{
 vi.mocked(runtimeRequest).mockRejectedValueOnce({code:"transport_lost",message:"Disconnected"});
 render(<RuntimeConversation session={session} backendKey="local" onConfiguration={vi.fn()}/>);
 fireEvent.change(screen.getByLabelText("Runtime message"),{target:{value:"one prompt"}});fireEvent.click(screen.getByRole("button",{name:"Send runtime message"}));
 await screen.findByText("Disconnected");expect(runtimeRequest).toHaveBeenCalledTimes(1);
 vi.mocked(runtimeRequest).mockResolvedValueOnce({executionId:"first"});fireEvent.click(screen.getByRole("button",{name:"Check submission receipt"}));await waitFor(()=>expect(runtimeRequest).toHaveBeenCalledTimes(2));expect(vi.mocked(runtimeRequest).mock.calls[1][0]).toBe("execution");
});
it("routes an explicit Codex steer to the expected execution without submitting a new prompt",async()=>{
 const active={executionId:"first",requestId:"request",sessionId:"session",submission:"accepted" as const,execution:"running" as const,nativeTurnId:"native-turn",input:{requestId:"request",sessionId:"session",prompt:[]},error:null};
 vi.mocked(useRuntimeConversation).mockReturnValue({events:[],executions:[active],error:null,reload:vi.fn()});
 const codex={...session,runtime:{...session.runtime,kind:"codex" as const},capabilities:{...session.capabilities,steering:true}};
 render(<RuntimeConversation session={codex} backendKey="local" onConfiguration={vi.fn()}/>);
 fireEvent.change(screen.getByLabelText("Runtime follow-up"),{target:{value:"steer"}});fireEvent.change(screen.getByLabelText("Runtime message"),{target:{value:"steer input"}});fireEvent.click(screen.getByRole("button",{name:"Steer runtime execution"}));
 await waitFor(()=>expect(runtimeRequest).toHaveBeenCalledWith("steer",{sessionId:"session",executionId:"first",prompt:[{type:"text",text:"steer input"}]}));expect(runtimeRequest).toHaveBeenCalledTimes(1);
});

it("renders the real Codex session contract with null configuration",()=>{
 const codex={...session,profile:{...session.profile,name:"Codex",kind:"codex" as const},runtime:{...session.runtime,kind:"codex" as const},configuration:null,capabilities:{...session.capabilities,steering:true}};
 render(<RuntimeConversation session={codex} backendKey="local" onConfiguration={vi.fn()}/>);
 expect(screen.getByLabelText("Runtime message")).toBeTruthy();
 expect(screen.queryByLabelText("Runtime Model")).toBeNull();
});
