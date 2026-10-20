// @vitest-environment jsdom
import {cleanup,fireEvent,render,screen,waitFor} from "@testing-library/react";
import {afterEach,beforeEach,expect,it,vi} from "vitest";
import {openUrl} from "@tauri-apps/plugin-opener";
import {authenticateRuntimeConnection,cancelRuntimeConnectionOperation,closeRuntimeConnection,getRuntimeConnectionOperation,getRuntimeConnectionStatus,prepareRuntimeConnection,readAcpLoginTerminal,respondAcpClientRequest,resizeAcpLoginTerminal,writeAcpLoginTerminal} from "@services/tauri";
import {AcpConnectionControls} from "./AcpConnectionControls";

vi.mock("@tauri-apps/plugin-opener",()=>({openUrl:vi.fn()}));
vi.mock("@services/tauri",()=>({
  authenticateRuntimeConnection:vi.fn(),cancelRuntimeConnectionOperation:vi.fn(),closeAcpLoginTerminal:vi.fn(),closeRuntimeConnection:vi.fn(),
  getRuntimeConnectionOperation:vi.fn(),getRuntimeConnectionStatus:vi.fn(),logoutRuntimeConnection:vi.fn(),
  prepareRuntimeConnection:vi.fn(),readAcpLoginTerminal:vi.fn(),respondAcpClientRequest:vi.fn(),
  resizeAcpLoginTerminal:vi.fn(),writeAcpLoginTerminal:vi.fn(),
}));
afterEach(()=>{cleanup();vi.resetAllMocks();});
beforeEach(()=>{
  vi.mocked(openUrl).mockResolvedValue(undefined);
  vi.mocked(resizeAcpLoginTerminal).mockResolvedValue(undefined);
  vi.mocked(writeAcpLoginTerminal).mockResolvedValue(undefined);
  vi.mocked(closeRuntimeConnection).mockResolvedValue(undefined);
  vi.mocked(cancelRuntimeConnectionOperation).mockResolvedValue({operationId:"op",connectionId:"connection",generation:1,state:"cancelled"});
});
const profile={id:"claude",revision:4,name:"Claude",kind:"acp" as const,executable:"npx",args:["--yes","@agentclientprotocol/claude-agent-acp@0.94.0"]};
const capabilities={loadSession:false,sessionDiscovery:false,images:false,configuration:false,cancellation:false,steering:false,permissions:false};

it("prepares a profile connection, advertises auth methods, and requests authentication by method ID",async()=>{
  vi.mocked(prepareRuntimeConnection).mockResolvedValue({operationId:"prepare",connectionId:"connection",generation:2,state:"succeeded"});
  vi.mocked(getRuntimeConnectionStatus).mockResolvedValue({connectionId:"connection",generation:2,profileId:"claude",agentInfo:{name:"Claude ACP"},capabilities,authMethods:[{id:"login",name:"Sign in",type:"agent"}],authState:"unauthenticated",pendingClientRequests:[]});
  vi.mocked(authenticateRuntimeConnection).mockResolvedValue({operationId:"auth",connectionId:"connection",generation:2,state:"pending"});
  vi.mocked(getRuntimeConnectionOperation).mockResolvedValue({operationId:"auth",connectionId:"connection",generation:2,state:"succeeded"});
  render(<AcpConnectionControls profile={profile}/>);
  fireEvent.click(screen.getByRole("button",{name:"Connect Claude"}));
  await screen.findByRole("button",{name:"Sign in"});
  expect(prepareRuntimeConnection).toHaveBeenCalledWith({profileId:"claude",expectedProfileRevision:4,role:"authentication",requestId:expect.any(String)});
  fireEvent.click(screen.getByRole("button",{name:"Sign in"}));
  await waitFor(()=>expect(authenticateRuntimeConnection).toHaveBeenCalledWith({connectionId:"connection",generation:2,methodId:"login",requestId:expect.any(String)}));
});

it("requires user action to open and separately consent to a sign-in URL",async()=>{
  vi.mocked(prepareRuntimeConnection).mockResolvedValue({operationId:"prepare",connectionId:"connection",generation:1,state:"succeeded"});
  vi.mocked(getRuntimeConnectionStatus).mockResolvedValue({connectionId:"connection",generation:1,profileId:"claude",agentInfo:{},capabilities,authMethods:[],authState:"awaiting_user",pendingClientRequests:[{clientRequestId:"elicitation-7",method:"elicitation/create",params:{url:"https://example.test/device",message:"Authorize Claude"}}]});
  render(<AcpConnectionControls profile={profile}/>);
  fireEvent.click(screen.getByRole("button",{name:"Connect Claude"}));
  await screen.findByText("https://example.test/device");
  fireEvent.click(screen.getByRole("button",{name:"Open sign-in URL"}));
  await waitFor(()=>expect(openUrl).toHaveBeenCalledWith("https://example.test/device"));
  expect(respondAcpClientRequest).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button",{name:"Confirm URL sign-in"}));
  await waitFor(()=>expect(respondAcpClientRequest).toHaveBeenCalledWith({connectionId:"connection",generation:1,clientRequestId:"elicitation-7",decision:{action:"accept"}}));
});

it("shows terminal authentication output and sequences user input through the backend",async()=>{
  vi.mocked(prepareRuntimeConnection).mockResolvedValue({operationId:"prepare",connectionId:"connection",generation:3,state:"succeeded"});
  vi.mocked(getRuntimeConnectionStatus).mockResolvedValue({connectionId:"connection",generation:3,profileId:"claude",agentInfo:{},capabilities,authMethods:[{id:"terminal-login",name:"Terminal sign-in",type:"terminal"}],authState:"unauthenticated",pendingClientRequests:[]});
  vi.mocked(authenticateRuntimeConnection).mockResolvedValue({operationId:"terminal-op",connectionId:"connection",generation:3,state:"awaiting_user"});
  vi.mocked(readAcpLoginTerminal).mockResolvedValue({output:"Enter device code: ",nextCursor:18,truncated:false,state:"running"});
  render(<AcpConnectionControls profile={profile}/>);
  fireEvent.click(screen.getByRole("button",{name:"Connect Claude"}));
  fireEvent.click(await screen.findByRole("button",{name:"Terminal sign-in"}));
  await screen.findByText("Enter device code:",{exact:false});
  fireEvent.change(screen.getByRole("textbox",{name:"Login terminal input"}),{target:{value:"device-42"}});
  fireEvent.click(screen.getByRole("button",{name:"Send login input"}));
  await waitFor(()=>expect(writeAcpLoginTerminal).toHaveBeenCalledWith({connectionId:"connection",generation:3,operationId:"terminal-op",inputSequence:1,data:btoa("device-42\n")}));
  fireEvent.click(screen.getByRole("button",{name:"Set terminal size"}));
  expect(resizeAcpLoginTerminal).toHaveBeenCalledWith({connectionId:"connection",generation:3,operationId:"terminal-op",columns:100,rows:30});
});

it("closes an idle prepared connection explicitly and cancels a pending preparation on unmount",async()=>{
  vi.mocked(prepareRuntimeConnection).mockResolvedValue({operationId:"prepared",connectionId:"connection",generation:6,state:"pending"});
  vi.mocked(getRuntimeConnectionStatus).mockResolvedValue({connectionId:"connection",generation:6,profileId:"claude",agentInfo:{},capabilities,authMethods:[],authState:"ready",pendingClientRequests:[]});
  const view=render(<AcpConnectionControls profile={profile} backendKey="remote:one"/>);
  fireEvent.click(screen.getByRole("button",{name:"Connect Claude"}));
  fireEvent.click(await screen.findByRole("button",{name:"Disconnect idle ACP connection"}));
  await waitFor(()=>expect(closeRuntimeConnection).toHaveBeenCalledWith({connectionId:"connection",generation:6}));
  expect(cancelRuntimeConnectionOperation).toHaveBeenCalledWith(expect.objectContaining({operationId:"prepared",connectionId:"connection",generation:6}));
  view.unmount();

  let finish:(value:{operationId:string;connectionId:string;generation:number;state:string})=>void=()=>{};
  vi.mocked(prepareRuntimeConnection).mockImplementationOnce(()=>new Promise(resolve=>{finish=resolve;}));
  vi.mocked(closeRuntimeConnection).mockClear();vi.mocked(cancelRuntimeConnectionOperation).mockClear();
  const pending=render(<AcpConnectionControls profile={{...profile,id:"claude-pending"}} backendKey="local"/>);
  fireEvent.click(screen.getByRole("button",{name:"Connect Claude"}));
  pending.unmount();
  finish({operationId:"late",connectionId:"late-connection",generation:8,state:"pending"});
  await waitFor(()=>expect(cancelRuntimeConnectionOperation).toHaveBeenCalledWith(expect.objectContaining({operationId:"late",connectionId:"late-connection",generation:8})));
  await waitFor(()=>expect(closeRuntimeConnection).toHaveBeenCalledWith({connectionId:"late-connection",generation:8}));
});

it("does not send an old connection ID to a newly selected backend",async()=>{
  vi.mocked(prepareRuntimeConnection).mockResolvedValue({operationId:"prepared",connectionId:"old-backend-connection",generation:9,state:"succeeded"});
  vi.mocked(getRuntimeConnectionStatus).mockResolvedValue({connectionId:"old-backend-connection",generation:9,profileId:"claude",agentInfo:{},capabilities,authMethods:[],authState:"ready",pendingClientRequests:[]});
  const view=render(<AcpConnectionControls profile={profile} backendKey="remote:one"/>);
  fireEvent.click(screen.getByRole("button",{name:"Connect Claude"}));
  await screen.findByText(/connection generation 9/);
  vi.mocked(getRuntimeConnectionStatus).mockClear();vi.mocked(closeRuntimeConnection).mockClear();
  view.rerender(<AcpConnectionControls profile={profile} backendKey="remote:two"/>);
  await waitFor(()=>expect(screen.getByRole("button",{name:"Connect Claude"})).toBeTruthy());
  expect(getRuntimeConnectionStatus).not.toHaveBeenCalled();
  expect(closeRuntimeConnection).not.toHaveBeenCalled();
});

it("keeps the connection UI when the backend reports that a conversation already owns it",async()=>{
  vi.mocked(prepareRuntimeConnection).mockResolvedValue({operationId:"prepared",connectionId:"conversation-owned",generation:10,state:"succeeded"});
  vi.mocked(getRuntimeConnectionStatus).mockResolvedValue({connectionId:"conversation-owned",generation:10,profileId:"claude",agentInfo:{},capabilities,authMethods:[],authState:"ready",pendingClientRequests:[]});
  vi.mocked(closeRuntimeConnection).mockRejectedValue({code:"runtime_busy",message:"Connection is owned by an active session"});
  render(<AcpConnectionControls profile={profile}/>);
  fireEvent.click(screen.getByRole("button",{name:"Connect Claude"}));
  fireEvent.click(await screen.findByRole("button",{name:"Disconnect idle ACP connection"}));
  await screen.findByRole("alert");
  expect(screen.getByText("Connection is owned by an active session")).toBeTruthy();
  expect(screen.queryByRole("button",{name:"Disconnect idle ACP connection"})).toBeTruthy();
});
