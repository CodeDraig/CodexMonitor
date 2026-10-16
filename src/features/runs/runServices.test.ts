import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { createRun, listRuns, startThread, sendUserMessage, captureRunInstance, saveRunComparison, listRunTurns } from "@services/tauri";
vi.mock("@tauri-apps/api/core", () => ({invoke: vi.fn()}));
beforeEach(() => {vi.clearAllMocks();vi.mocked(invoke).mockResolvedValue({});});
describe("run IPC contracts", () => {
  it("uses the shared input envelope and caller-owned request IDs", async () => {
    const input = {requestId: "request",workspaceId: "w",title: "Run",prompt: "Original",mode: "local" as const,images: [],instances: [{model: null}]};
    await createRun(input);
    expect(invoke).toHaveBeenLastCalledWith("create_run", {input});
    await listRuns("w", "cursor", 20);
    expect(invoke).toHaveBeenLastCalledWith("list_runs", {input: {workspaceId: "w",cursor: "cursor",limit: 20}});
    await captureRunInstance("i", "t2", "capture-request", "Later");
    expect(invoke).toHaveBeenLastCalledWith("capture_run_instance", {input: {instanceId: "i",turnId: "t2",requestId: "capture-request",label: "Later"}});
    await listRunTurns("i");
    expect(invoke).toHaveBeenLastCalledWith("list_run_turns", {input: {instanceId: "i"}});
    const comparison = {leftCaptureId: "a",rightCaptureId: "b",choice: "tie" as const,notes: "Evidence",expectedRevision: 2};
    await saveRunComparison(comparison);
    expect(invoke).toHaveBeenLastCalledWith("save_run_comparison", {input: comparison});
  });
  it("adds tracking without changing untracked payloads or omitted/null tier", async () => {
    const runContext = {runId: "run",instanceId: "instance"};
    await startThread("w");expect(invoke).toHaveBeenLastCalledWith("start_thread", {workspaceId: "w"});
    await startThread("w",runContext);expect(invoke).toHaveBeenLastCalledWith("start_thread", {workspaceId: "w",runContext});
    await sendUserMessage("w", "t", "text", {runContext});
    let payload = vi.mocked(invoke).mock.calls.slice(-1)[0]?.[1] as Record<string, unknown>;
    expect(payload.runContext).toEqual(runContext); expect(payload).not.toHaveProperty("serviceTier");
    await sendUserMessage("w", "t", "text", {runContext,serviceTier: null});
    payload = vi.mocked(invoke).mock.calls.slice(-1)[0]?.[1] as Record<string, unknown>;
    expect(payload.serviceTier).toBeNull();
  });
});
