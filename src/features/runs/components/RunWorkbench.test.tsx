// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { RunWorkbench } from "./RunWorkbench";
import { getRunCapture, listRunComparisons, listWorkspaces, saveRunComparison } from "@services/tauri";
import type { RunCapture, RunRecord } from "../types";
vi.mock("@services/tauri", () => ({getRunCapture: vi.fn(),listRunComparisons: vi.fn(),listWorkspaces: vi.fn(),saveRunComparison: vi.fn(),captureRunInstance: vi.fn(),listRunTurns: vi.fn(),reconcileRun: vi.fn()}));
vi.mock("@/features/git/components/GitDiffViewer", () => ({GitDiffViewer: () => <div>Saved patches</div>}));
const run: RunRecord = {schemaVersion: 1, storeId: "s", id: "r", requestId: "q", workspaceId: "w", workspaceName: "Workspace", workspacePath: "/gone", title: "Recorded run", prompt: "Prompt", mode: "local", createdAt: 1, revision: 0, baseOid: "base", baseline: {available: true, files: [], errors: []}, attachments: [], instances: [0, 1].map(ordinal => ({id: `i${ordinal}`, runId: "r", ordinal, requested: {modelLabel: `Model ${ordinal}`}, preparation: "submitted", workspaceId: "gone", workspacePath: "/gone", threadId: `t${ordinal}`, initialTurnId: "initial", submissionOutcome: ordinal ? "unknown" : "accepted", executionOutcome: "completed", submission: null, baseline: null, setup: {status: "dispatched"}, error: null, captures: [{id: `c${ordinal}`, kind: "initial_turn", turnId: "initial", label: `Capture ${ordinal}`, createdAt: 1, complete: ordinal === 0}]}))};
afterEach(cleanup);
beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(listWorkspaces).mockResolvedValue([]);
  vi.mocked(listRunComparisons).mockResolvedValue([]);
  vi.mocked(getRunCapture).mockImplementation(async id => ({...run.instances[id === "c0" ? 0 : 1].captures[0], schemaVersion: 1, instanceId: id === "c0" ? "i0" : "i1", requestId: id, status: "completed", items: [{type: "agentMessage", text: "tests passed"}], usage: null, observed: {}, git: {available: true, files: [], errors: []}, errors: id === "c1" ? ["History page unavailable"] : []} as RunCapture));
});
describe("RunWorkbench", () => {
  it("retains evidence for missing workspaces and shows unknown/incomplete states", async () => {
    render(<RunWorkbench runs={[run]} onSelectInstance={vi.fn()} />);
    expect(await screen.findAllByText(/Live workspace missing/)).toHaveLength(2);
    expect(screen.getByText(/Submission: unknown/)).toBeTruthy();
    fireEvent.change(screen.getByLabelText("Left capture"), {target: {value: "c0"}});
    fireEvent.change(screen.getByLabelText("Right capture"), {target: {value: "c1"}});
    expect(await screen.findByText("History page unavailable")).toBeTruthy();
    fireEvent.click(screen.getByRole("tab", {name: "Tokens"}));
    expect(screen.getAllByText("Turn-attributable token usage unavailable")).toHaveLength(2);
    fireEvent.click(screen.getByRole("tab", {name: "Commands"}));
    expect(screen.queryByText("tests passed")).toBeNull();
    expect(screen.getAllByText("Setup: dispatched")).toHaveLength(2);
  });
  it("loads saved choice and notes, preserves drafts on conflict, and sends revision", async () => {
    vi.mocked(listRunComparisons).mockResolvedValue([{id: "pair",leftCaptureId: "c0",rightCaptureId: "c1",choice: "left",notes: "saved",revision: 3,updatedAt: 1}]);
    vi.mocked(saveRunComparison).mockRejectedValue(new Error("conflict: comparison notes changed"));
    render(<RunWorkbench runs={[run]} onSelectInstance={vi.fn()} />);
    fireEvent.change(screen.getByLabelText("Left capture"), {target: {value: "c0"}});
    fireEvent.change(screen.getByLabelText("Right capture"), {target: {value: "c1"}});
    const notes = await screen.findByLabelText("Comparison notes");
    await waitFor(() => expect((notes as HTMLTextAreaElement).value).toBe("saved"));
    fireEvent.change(notes, {target: {value: "my draft"}});
    fireEvent.click(screen.getByRole("button", {name: "Save comparison"}));
    expect(await screen.findByRole("alert")).toHaveProperty("textContent", "Error: conflict: comparison notes changed");
    expect((notes as HTMLTextAreaElement).value).toBe("my draft");
    expect(saveRunComparison).toHaveBeenCalledWith({leftCaptureId: "c0",rightCaptureId: "c1",choice: "left",notes: "my draft",expectedRevision: 3});
  });
});
