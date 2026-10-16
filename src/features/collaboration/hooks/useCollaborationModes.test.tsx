// @vitest-environment jsdom
import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { WorkspaceInfo } from "../../../types";
import { getCollaborationModes } from "../../../services/tauri";
import { useCollaborationModes } from "./useCollaborationModes";

vi.mock("../../../services/tauri", () => ({
  getCollaborationModes: vi.fn(),
}));

const workspaceOne: WorkspaceInfo = {
  id: "workspace-1",
  name: "Workspace One",
  path: "/tmp/workspace-one",
  connected: true,
  settings: { sidebarCollapsed: false },
};

const workspaceTwoDisconnected: WorkspaceInfo = {
  id: "workspace-2",
  name: "Workspace Two",
  path: "/tmp/workspace-two",
  connected: false,
  settings: { sidebarCollapsed: false },
};

const workspaceTwoConnected: WorkspaceInfo = {
  ...workspaceTwoDisconnected,
  connected: true,
};

const makeModesResponse = () => ({
  result: {
    data: [{ name: "Plan", mode: "plan", model: null, reasoning_effort: null }, { name: "Default", mode: "default", model: null, reasoning_effort: null }],
  },
});

describe("useCollaborationModes", () => {
  afterEach(() => {
    vi.clearAllMocks();
  });

  it("keeps the last selected mode across workspace switches and reconnects", async () => {
    vi.mocked(getCollaborationModes).mockImplementation(async () => makeModesResponse());

    const { result, rerender } = renderHook(
      ({ workspace }: { workspace: WorkspaceInfo | null }) =>
        useCollaborationModes({ activeWorkspace: workspace }),
      {
        initialProps: { workspace: workspaceOne },
      },
    );

    await waitFor(() => expect(result.current.selectedCollaborationModeId).toBe("default"));

    act(() => {
      result.current.setSelectedCollaborationModeId("plan");
    });
    expect(result.current.selectedCollaborationModeId).toBe("plan");

    rerender({ workspace: workspaceTwoDisconnected });
    expect(result.current.selectedCollaborationModeId).toBe("plan");
    expect(result.current.collaborationModes).toEqual([]);

    rerender({ workspace: workspaceTwoConnected });

    await waitFor(() => {
      expect(getCollaborationModes).toHaveBeenCalledWith("workspace-2");
      expect(result.current.selectedCollaborationModeId).toBe("plan");
    });
  });


  it("resets to the workspace default when selectionKey changes and preferredModeId is null", async () => {
    vi.mocked(getCollaborationModes).mockResolvedValue(makeModesResponse());

    const { result, rerender } = renderHook(
      ({
        workspace,
        preferredModeId,
        selectionKey,
      }: {
        workspace: WorkspaceInfo | null;
        preferredModeId: string | null;
        selectionKey: string | null;
      }) =>
        useCollaborationModes({
          activeWorkspace: workspace,
          preferredModeId,
          selectionKey,
        }),
      {
        initialProps: {
          workspace: workspaceOne,
          preferredModeId: "default" as string | null,
          selectionKey: "thread-a",
        },
      },
    );

    await waitFor(() => expect(result.current.selectedCollaborationModeId).toBe("default"));

    act(() => {
      result.current.setSelectedCollaborationModeId("plan");
    });
    expect(result.current.selectedCollaborationModeId).toBe("plan");

    // Thread switch with no stored override: preferredModeId is null.
    rerender({
      workspace: workspaceOne,
      preferredModeId: null,
      selectionKey: "thread-b",
    });

    expect(result.current.selectedCollaborationModeId).toBe("default");
  });

  it("falls back to the workspace default when the preferredModeId is stale", async () => {
    vi.mocked(getCollaborationModes).mockResolvedValue(makeModesResponse());

    const { result, rerender } = renderHook(
      (props: {
        preferredModeId: string | null;
        selectionKey: string;
      }) =>
        useCollaborationModes({
          activeWorkspace: workspaceOne,
          preferredModeId: props.preferredModeId,
          selectionKey: props.selectionKey,
        }),
      {
        initialProps: {
          preferredModeId: "plan",
          selectionKey: "thread-a",
        },
      },
    );

    await waitFor(() => {
      expect(result.current.collaborationModes.length).toBeGreaterThan(0);
    });
    expect(result.current.selectedCollaborationModeId).toBe("plan");

    rerender({
      preferredModeId: "stale-mode-id",
      selectionKey: "thread-b",
    });

    await waitFor(() => {
      expect(result.current.selectedCollaborationModeId).toBe("default");
    });
  });


});
