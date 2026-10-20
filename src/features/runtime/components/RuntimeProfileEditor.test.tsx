// @vitest-environment jsdom
import {cleanup,fireEvent,render,screen,waitFor} from "@testing-library/react";
import {afterEach,expect,it,vi} from "vitest";
import {runtimeRequest} from "@services/tauri";
import {RuntimeProfileEditor} from "./RuntimeProfileEditor";

vi.mock("@services/tauri",()=>({runtimeRequest:vi.fn()}));
afterEach(()=>{cleanup();vi.resetAllMocks();});

it("saves the pinned Claude ACP launch recipe, backend-only environment references, and callback policy",async()=>{
  const onSaved=vi.fn(async()=>{});vi.mocked(runtimeRequest).mockResolvedValue(undefined);
  render(<RuntimeProfileEditor profiles={[]} onSaved={onSaved}/>);
  fireEvent.click(screen.getByRole("button",{name:"Claude Agent ACP 0.94.0 preset"}));
  expect((screen.getByLabelText("ACP executable") as HTMLInputElement).value).toBe("claude-agent-acp");
  expect((screen.getByLabelText("ACP arguments (JSON array)") as HTMLInputElement).value).toBe("[]");
  fireEvent.change(screen.getByLabelText("Environment sources (JSON; literal values or backend variable names)"),{target:{value:JSON.stringify({ANTHROPIC_API_KEY:{kind:"backendEnv",name:"CLAUDE_TOKEN"}})}});
  fireEvent.change(screen.getByLabelText("Client filesystem callbacks"),{target:{value:"read_write"}});
  fireEvent.change(screen.getByLabelText("Client command execution"),{target:{value:"ask"}});
  fireEvent.change(screen.getByLabelText("Additional filesystem roots (one backend path per line)"),{target:{value:"/srv/agent-data\n/opt/work"}});
  fireEvent.click(screen.getByRole("button",{name:"Save ACP profile"}));
  await waitFor(()=>expect(runtimeRequest).toHaveBeenCalledWith("save_profile",expect.objectContaining({profile:expect.objectContaining({
    id:"claude-acp",kind:"acp",presetId:"claude-acp",executable:"claude-agent-acp",args:[],
    environment:{ANTHROPIC_API_KEY:{kind:"backendEnv",name:"CLAUDE_TOKEN"}},
    clientPolicy:{filesystem:"read_write",commandExecution:"ask",additionalRoots:["/srv/agent-data","/opt/work"]},
  })})));
  expect(JSON.stringify(vi.mocked(runtimeRequest).mock.calls)).not.toContain("secret-value");
  expect(onSaved).toHaveBeenCalledOnce();
});

it("uses the installed Gemini executable and explains its callback requirements",()=>{
  render(<RuntimeProfileEditor profiles={[]} onSaved={async()=>{}}/>);
  fireEvent.click(screen.getByRole("button",{name:"Gemini CLI 0.64.0 preset"}));
  expect((screen.getByLabelText("ACP executable") as HTMLInputElement).value).toBe("gemini");
  expect((screen.getByLabelText("ACP arguments (JSON array)") as HTMLInputElement).value).toBe('["--acp"]');
  expect(screen.getByRole("note").textContent).toContain("requires filesystem and terminal callbacks");
});

it("loads an existing OpenCode profile without dropping its implicit ACP launch argument",()=>{
  const profile={id:"opencode",revision:3,name:"OpenCode",kind:"open_code_acp" as const,executable:"opencode",args:[],presetId:"opencode",clientPolicy:{filesystem:"disabled" as const,commandExecution:"disabled" as const,additionalRoots:[]}};
  render(<RuntimeProfileEditor profiles={[profile]} onSaved={async()=>{}}/>);
  fireEvent.change(screen.getByLabelText("Edit profile"),{target:{value:"opencode"}});
  expect((screen.getByLabelText("ACP arguments (JSON array)") as HTMLInputElement).value).toBe('["acp"]');
});
