import { parseCollaborationModes } from "@/features/collaboration/utils/collaborationModes";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type {
  CollaborationModeOption,
  DebugEntry,
  WorkspaceInfo,
} from "../../../types";
import { getCollaborationModes } from "../../../services/tauri";

type UseCollaborationModesOptions = {
  activeWorkspace: WorkspaceInfo | null;
  preferredModeId?: string | null;
  selectionKey?: string | null;
  onDebug?: (entry: DebugEntry) => void;
};

function pickWorkspaceDefaultModeId(modes: CollaborationModeOption[]): string | null {
  return (
    modes.find(
      (mode) =>
        mode.id.trim().toLowerCase() === "default" ||
        mode.mode.trim().toLowerCase() === "default",
    )?.id ??
    modes.find(
      (mode) =>
        mode.id.trim().toLowerCase() === "code" ||
        mode.mode.trim().toLowerCase() === "code",
    )?.id ??
    modes[0]?.id ??
    null
  );
}

export function useCollaborationModes({
  activeWorkspace,
  preferredModeId = null,
  selectionKey = null,
  onDebug,
}: UseCollaborationModesOptions) {
  const [modes, setModes] = useState<CollaborationModeOption[]>([]);
  const [selectedModeId, setSelectedModeId] = useState<string | null>(null);
  const lastFetchedWorkspaceId = useRef<string | null>(null);
  const previousWorkspaceId = useRef<string | null>(null);
  const inFlight = useRef(false);
  const selectedModeIdRef = useRef<string | null>(null);
  const lastSelectionKey = useRef<string | null>(null);

  const workspaceId = activeWorkspace?.id ?? null;
  const isConnected = Boolean(activeWorkspace?.connected);

  const selectedMode = useMemo(
    () => modes.find((mode) => mode.id === selectedModeId) ?? null,
    [modes, selectedModeId],
  );

  const refreshModes = useCallback(async () => {
    if (!workspaceId || !isConnected) {
      return;
    }
    if (inFlight.current) {
      return;
    }
    inFlight.current = true;
    onDebug?.({
      id: `${Date.now()}-client-collaboration-mode-list`,
      timestamp: Date.now(),
      source: "client",
      label: "collaborationMode/list",
      payload: { workspaceId },
    });
    try {
      const response = await getCollaborationModes(workspaceId);
      onDebug?.({
        id: `${Date.now()}-server-collaboration-mode-list`,
        timestamp: Date.now(),
        source: "server",
        label: "collaborationMode/list response",
        payload: response,
      });
      const data = parseCollaborationModes(response);
      setModes(data);
      lastFetchedWorkspaceId.current = workspaceId;
      const workspaceDefaultModeId = pickWorkspaceDefaultModeId(data);
      setSelectedModeId((currentSelection) => {
        const selection = currentSelection ?? selectedModeIdRef.current;
        if (!selection) {
          return workspaceDefaultModeId;
        }
        if (!data.some((mode) => mode.id === selection)) {
          return workspaceDefaultModeId;
        }
        return selection;
      });
    } catch (error) {
      onDebug?.({
        id: `${Date.now()}-client-collaboration-mode-list-error`,
        timestamp: Date.now(),
        source: "error",
        label: "collaborationMode/list error",
        payload: error instanceof Error ? error.message : String(error),
      });
    } finally {
      inFlight.current = false;
    }
  }, [isConnected, onDebug, workspaceId]);

  useEffect(() => {
    selectedModeIdRef.current = selectedModeId;
  }, [selectedModeId]);

  useEffect(() => {
    if (selectionKey === lastSelectionKey.current) {
      return;
    }
    lastSelectionKey.current = selectionKey;
    // When switching threads, prefer the per-thread override. If there is no stored override,
    // reset to the workspace default instead of carrying over the previous thread's selection.
    // Also validate that a stored override still exists; otherwise fall back to the workspace default
    // so collaboration payload generation remains enabled.
    setSelectedModeId(() => {
      if (!modes.length) {
        // If modes aren't loaded yet, keep the preferred ID (if any) until refresh validates it.
        return preferredModeId;
      }
      if (preferredModeId && modes.some((mode) => mode.id === preferredModeId)) {
        return preferredModeId;
      }
      return pickWorkspaceDefaultModeId(modes);
    });
  }, [modes, preferredModeId, selectionKey]);

  useEffect(() => {
    if (previousWorkspaceId.current !== workspaceId) {
      previousWorkspaceId.current = workspaceId;
      setModes([]);
      lastFetchedWorkspaceId.current = null;
    }
  }, [workspaceId]);

  useEffect(() => {
    if (!workspaceId || !isConnected) {
      setModes([]);
      lastFetchedWorkspaceId.current = null;
      return;
    }
    const alreadyFetchedForWorkspace = lastFetchedWorkspaceId.current === workspaceId;
    if (alreadyFetchedForWorkspace) {
      return;
    }
    refreshModes();
  }, [isConnected, modes.length, refreshModes, workspaceId]);

  return {
    collaborationModes: modes,
    selectedCollaborationMode: selectedMode,
    selectedCollaborationModeId: selectedModeId,
    setSelectedCollaborationModeId: setSelectedModeId,
    refreshCollaborationModes: refreshModes,
  };
}
