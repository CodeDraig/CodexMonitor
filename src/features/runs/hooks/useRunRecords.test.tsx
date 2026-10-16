// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { listRuns } from "@services/tauri";
import { useRunRecords } from "./useRunRecords";
import type { RunPage } from "../types";
vi.mock("@services/tauri", () => ({listRuns: vi.fn(), reconcileRun: vi.fn()}));
afterEach(cleanup);
describe("useRunRecords", () => {
  it("ignores a previous backend response even with overlapping workspace IDs", async () => {
    let oldResponse!: (page: RunPage) => void;
    vi.mocked(listRuns).mockReturnValueOnce(new Promise(resolve => {oldResponse = resolve;})).mockResolvedValue({storeId: "new-store", runs: [], nextCursor: null});
    const {result, rerender} = renderHook(({backend}) => useRunRecords("same-workspace", backend), {initialProps: {backend: "old-host"}});
    rerender({backend: "new-host"});
    await waitFor(() => expect(result.current.storeId).toBe("new-store"));
    await act(async () => oldResponse({storeId: "old-store", runs: [], nextCursor: null}));
    expect(result.current.storeId).toBe("new-store");
  });
  it("reports a failed store read instead of silently presenting empty history", async () => {
    vi.mocked(listRuns).mockRejectedValue(new Error("Unsupported run schema 99"));
    const {result} = renderHook(() => useRunRecords("w", "local"));
    await waitFor(() => expect(result.current.error).toContain("Unsupported run schema"));
    expect(result.current.loading).toBe(false);
  });
});
