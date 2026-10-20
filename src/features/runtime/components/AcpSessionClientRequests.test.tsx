// @vitest-environment jsdom
import {cleanup,fireEvent,render,screen,waitFor} from "@testing-library/react";
import {afterEach,expect,it,vi} from "vitest";
import {AcpSessionClientRequests} from "./AcpSessionClientRequests";
import {getRuntimeSessionConnectionStatus,respondAcpClientRequest,runtimeRequest} from "@services/tauri";
import {openUrl} from "@tauri-apps/plugin-opener";
import type {AcpConnectionStatus} from "../types";

vi.mock("@services/tauri",()=>({
  getRuntimeSessionConnectionStatus:vi.fn(),
  respondAcpClientRequest:vi.fn(),
  runtimeRequest:vi.fn(),
}));
vi.mock("@tauri-apps/plugin-opener",()=>({openUrl:vi.fn().mockResolvedValue(undefined)}));
afterEach(()=>{cleanup();vi.clearAllMocks();});

const connection={connectionId:"conn-1",generation:4,profileId:"claude",agentInfo:{},capabilities:{},authMethods:[],authState:"authenticated",pendingClientRequests:[
  {clientRequestId:"approve-1",kind:"approval",method:"fs/write_text_file",command:"write file",args:["notes.txt"],cwd:"/work",message:"Allow this write?"},
  {clientRequestId:"url-1",kind:"url",method:"auth/open_url",url:"https://example.test/login",message:"Sign in"},
]};

it("looks up and responds to the selected live session without preparing or closing a connection",async()=>{
  vi.mocked(getRuntimeSessionConnectionStatus).mockResolvedValue(connection as never);
  vi.mocked(respondAcpClientRequest).mockResolvedValue(undefined as never);
  render(<AcpSessionClientRequests sessionId="session-1" backendKey="local"/>);
  await screen.findByText("Connected as claude · generation 4");
  expect(getRuntimeSessionConnectionStatus).toHaveBeenCalledWith("session-1");
  expect(screen.getByText("Arguments:").textContent).toContain("notes.txt");
  fireEvent.click(screen.getByRole("button",{name:"Allow once"}));
  await waitFor(()=>expect(respondAcpClientRequest).toHaveBeenCalledWith({connectionId:"conn-1",generation:4,clientRequestId:"approve-1",decision:{action:"allow_once"}}));
  expect(runtimeRequest).not.toHaveBeenCalled();
});

it("requires explicit user action to open a URL and separately consent",async()=>{
  vi.mocked(getRuntimeSessionConnectionStatus).mockResolvedValue(connection as never);
  vi.mocked(respondAcpClientRequest).mockResolvedValue(undefined as never);
  render(<AcpSessionClientRequests sessionId="session-1" backendKey="local"/>);
  await screen.findByText("Connected as claude · generation 4");
  fireEvent.click(screen.getAllByRole("button",{name:"Open destination"})[0]);
  await waitFor(()=>expect(openUrl).toHaveBeenCalledWith("https://example.test/login"));
  expect(respondAcpClientRequest).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button",{name:"Confirm consent"}));
  await waitFor(()=>expect(respondAcpClientRequest).toHaveBeenCalledWith({connectionId:"conn-1",generation:4,clientRequestId:"url-1",decision:{action:"accept"}}));
  expect(runtimeRequest).not.toHaveBeenCalled();
});

it("invalidates the previous session lookup when the selected backend and conversation change",async()=>{
  let resolveFirst!:(value:AcpConnectionStatus|null)=>void;
  vi.mocked(getRuntimeSessionConnectionStatus)
    .mockImplementationOnce(()=>new Promise(resolve=>{resolveFirst=resolve;}))
    .mockResolvedValue(connection as never);
  const view=render(<AcpSessionClientRequests sessionId="old-session" backendKey="local"/>);
  view.rerender(<AcpSessionClientRequests sessionId="new-session" backendKey="daemon"/>);
  await screen.findByText("Connected as claude · generation 4");
  resolveFirst({...connection,profileId:"stale-local"} as unknown as AcpConnectionStatus);
  await waitFor(()=>expect(getRuntimeSessionConnectionStatus).toHaveBeenCalledWith("new-session"));
  expect(screen.queryByText(/stale-local/)).toBeNull();
  expect(respondAcpClientRequest).not.toHaveBeenCalled();
  expect(runtimeRequest).not.toHaveBeenCalled();
});

it("shows the neutral waiting state for an idle cached conversation without reconnecting it",async()=>{
  vi.mocked(getRuntimeSessionConnectionStatus).mockRejectedValue("transport_lost: live ACP connection for Monitor session not found");
  render(<AcpSessionClientRequests sessionId="session-1" backendKey="local"/>);
  expect(await screen.findByText("Waiting for the foreground ACP connection.")).toBeTruthy();
  expect(screen.queryByRole("alert")).toBeNull();
  expect(runtimeRequest).not.toHaveBeenCalled();
});

it("keeps the missing-connection error visible while an execution is active",async()=>{
  vi.mocked(getRuntimeSessionConnectionStatus).mockRejectedValue("transport_lost: live ACP connection for Monitor session not found");
  render(<AcpSessionClientRequests sessionId="session-1" backendKey="local" activeExecution/>);
  expect((await screen.findByRole("alert")).textContent).toContain("transport_lost: live ACP connection for Monitor session not found");
  expect(screen.queryByText("Waiting for the foreground ACP connection.")).toBeNull();
});

it("does not suppress unrelated transport errors",async()=>{
  vi.mocked(getRuntimeSessionConnectionStatus).mockRejectedValue("transport_lost: daemon connection unavailable");
  render(<AcpSessionClientRequests sessionId="session-1" backendKey="local"/>);
  expect((await screen.findByRole("alert")).textContent).toContain("transport_lost: daemon connection unavailable");
  expect(screen.queryByText("Waiting for the foreground ACP connection.")).toBeNull();
});
