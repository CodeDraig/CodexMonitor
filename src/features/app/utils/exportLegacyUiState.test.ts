// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";

const { exportLegacyUiStateSnapshot } = vi.hoisted(() => ({
  exportLegacyUiStateSnapshot: vi.fn(),
}));
vi.mock("@/services/tauri", () => ({ exportLegacyUiStateSnapshot }));

import {
  collectLegacyUiStateSnapshot,
  exportLegacyUiState,
} from "./exportLegacyUiState";

describe("legacy UI state export", () => {
  beforeEach(() => {
    window.localStorage.clear();
    exportLegacyUiStateSnapshot.mockReset();
  });

  it("exports only the allowlisted presentation state and filters malformed values", () => {
    window.localStorage.setItem("codexmonitor.sidebarWidth", "312");
    window.localStorage.setItem("codexmonitor.sidebarCollapsed", "true");
    window.localStorage.setItem("codexmonitor.collapsedGroups", JSON.stringify(["group", 3]));
    window.localStorage.setItem("codexmonitor.pinnedThreads", JSON.stringify({ "ws:thread": 12, bad: "later" }));
    window.localStorage.setItem("codexmonitor.threadCustomNames", JSON.stringify({ "ws:thread": "Renamed" }));
    window.localStorage.setItem("codexmonitor.threadCodexParams", JSON.stringify({ "ws:thread": { modelId: "m1", apiKey: "private" }, bad: null }));
    window.localStorage.setItem("codexmonitor.promptHistory.workspace", JSON.stringify(["one", 4, "two"]));
    window.localStorage.setItem("codexmonitor.pendingPostUpdateVersion", "secret-ish metadata");
    window.localStorage.setItem("codexmonitor.detachedReviewLinks", JSON.stringify({ secret: "not exported" }));

    const snapshot = collectLegacyUiStateSnapshot(100);
    expect(snapshot).toEqual({
      schemaVersion: 1,
      exportedAtMs: 100,
      state: {
        schemaVersion: 1,
        revision: 0,
        panels: { sidebarWidth: 312, sidebarCollapsed: true },
        collapsedGroups: { group: true },
        threadListSortKey: null,
        threadListOrganizeMode: null,
        pinnedThreads: { "ws:thread": 12 },
        customNames: { "ws:thread": "Renamed" },
        threadCodexParams: { "ws:thread": { modelId: "m1" } },
        promptHistory: { workspace: ["one", "two"] },
      },
    });
  });

  it("writes the snapshot through the Rust IPC command", async () => {
    window.localStorage.setItem("codexmonitor.threadListSortKey", "created_at");
    exportLegacyUiStateSnapshot.mockResolvedValue({ path: "/data/legacy-ui-state-v1/snapshot.json" });
    await expect(exportLegacyUiState()).resolves.toBe("/data/legacy-ui-state-v1/snapshot.json");
    expect(exportLegacyUiStateSnapshot).toHaveBeenCalledWith(
      expect.objectContaining({
        schemaVersion: 1,
        state: expect.objectContaining({ threadListSortKey: "created_at" }),
      }),
    );
    expect(window.localStorage.getItem("codexmonitor.threadListSortKey")).toBe("created_at");
  });
});
