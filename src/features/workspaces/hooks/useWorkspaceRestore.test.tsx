// @vitest-environment jsdom
import {renderHook,waitFor} from "@testing-library/react";
import {expect,it,vi} from "vitest";
import {useWorkspaceRestore} from "./useWorkspaceRestore";
it("restores Codex workspaces while ACP-only registration starts no Codex process",async()=>{
 const acp={id:"acp",name:"ACP",path:"/acp",connected:false,settings:{sidebarCollapsed:false,codexAutoConnect:false}};
 const codex={id:"codex",name:"Codex",path:"/codex",connected:false,settings:{sidebarCollapsed:false}};
 const connectWorkspace=vi.fn().mockResolvedValue(undefined),listThreadsForWorkspaces=vi.fn().mockResolvedValue(undefined);
 renderHook(()=>useWorkspaceRestore({workspaces:[acp,codex],hasLoaded:true,connectWorkspace,listThreadsForWorkspaces}));
 await waitFor(()=>expect(listThreadsForWorkspaces).toHaveBeenCalled());expect(connectWorkspace).toHaveBeenCalledTimes(1);expect(connectWorkspace).toHaveBeenCalledWith(codex);
});
