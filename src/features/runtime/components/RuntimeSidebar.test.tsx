// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import type { WorkspaceInfo } from "@/types";
import type { AcpConversations } from "../hooks/useAcpConversations";
import { buildPrimaryNodes, type PrimaryLayoutNodesOptions } from "@/features/layout/hooks/layoutNodes/buildPrimaryNodes";

const workspace: WorkspaceInfo = { id: "workspace", name: "Project", path: "/project", connected: false, settings: { sidebarCollapsed: false } };
vi.mock("@app/components/Sidebar", () => ({
  Sidebar: ({ onAddAgent }: { onAddAgent: (workspace: WorkspaceInfo) => void }) => <button onClick={() => onAddAgent(workspace)}>Sidebar new agent</button>,
}));
vi.mock("./AgentSessionBrowser", () => ({ AgentSessionBrowser: () => null }));
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

function setup(overrides: Partial<AcpConversations> = {}) {
  const onCodex = vi.fn();
  const runtime = {
    backendKey: "local", selected: false,
    profiles: [
      { id: "codex", kind: "codex", name: "Codex", revision: 1, executable: "codex", args: [] },
      { id: "opencode", kind: "open_code_acp", name: "OpenCode", revision: 1, executable: "opencode", args: [] },
      { id: "other", kind: "acp", name: "Other agent", revision: 3, executable: "other", args: ["serve"] },
    ],
    refreshCatalog: vi.fn().mockResolvedValue(undefined),
    newConversation: vi.fn().mockResolvedValue(undefined), setBrowserOpen: vi.fn(),
    ...overrides,
  } as unknown as AcpConversations;
  const node = (current = runtime) => buildPrimaryNodes({
    runtime: current, sidebarProps: { onAddAgent: onCodex },
    desktopTopbarProps: {}, messagesProps: {},
  } as unknown as PrimaryLayoutNodesOptions).sidebarNode;
  const view = render(node());
  const open = async () => {
    fireEvent.click(screen.getByRole("button", { name: "Sidebar new agent" }));
    await waitFor(() => expect(screen.queryByText("Loading agent profiles…")).toBeNull());
  };
  return { ...view, runtime, onCodex, node, open };
}

it.each([false, true])("offers every configured agent independently of ACP selection (%s), without creating on open", async selected => {
  const { open, runtime, onCodex } = setup({ selected });
  await open();
  expect(screen.getByRole("dialog", { name: "New agent" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Codex" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "OpenCode · ACP" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Other agent · ACP" })).toBeTruthy();
  expect(onCodex).not.toHaveBeenCalled();
  expect(runtime.newConversation).not.toHaveBeenCalled();
});

it.each(["OpenCode · ACP", "Other agent · ACP"])("creates %s in the requested workspace without invoking Codex", async name => {
  const { open, runtime, onCodex } = setup();
  await open(); fireEvent.click(screen.getByRole("button", { name }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(runtime.newConversation).toHaveBeenCalledExactlyOnceWith("workspace", name.startsWith("OpenCode") ? "opencode" : "other");
  expect(onCodex).not.toHaveBeenCalled();
});

it("preserves the existing Codex workflow even with ACP selected", async () => {
  const { open, runtime, onCodex } = setup({ selected: true });
  await open(); fireEvent.click(screen.getByRole("button", { name: "Codex" }));
  expect(onCodex).toHaveBeenCalledExactlyOnceWith(workspace);
  expect(runtime.newConversation).not.toHaveBeenCalled();
});

it("keeps a failed ACP creation visible and retries the chosen profile; blocks duplicate creation while pending", async () => {
  let reject: (error: Error) => void = () => {};
  const create = vi.fn().mockImplementationOnce(() => new Promise((_, fail) => { reject = fail; })).mockResolvedValue(undefined);
  const { open } = setup({ newConversation: create });
  await open(); const button = screen.getByRole("button", { name: "OpenCode · ACP" });
  fireEvent.click(button); fireEvent.click(button);
  fireEvent.keyDown(document, { key: "Tab" });
  expect(screen.getByRole("dialog").contains(document.activeElement)).toBe(true);
  fireEvent.keyDown(document, { key: "Escape" });
  expect(create).toHaveBeenCalledTimes(1);
  expect(screen.getByRole("dialog")).toBeTruthy();
  await act(async () => reject(new Error("Agent executable unavailable")));
  expect(screen.getByRole("alert").textContent).toBe("Agent executable unavailable");
  fireEvent.click(button);
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(create).toHaveBeenCalledTimes(2);
});

it("does not silently create Codex when profile loading fails, and provides profile setup with no configured ACP agents", async () => {
  const refresh = vi.fn().mockRejectedValueOnce(new Error("Backend offline")).mockResolvedValue(undefined);
  const { open, runtime, onCodex } = setup({ profiles: [], refreshCatalog: refresh });
  await open();
  expect(screen.getByRole("alert").textContent).toBe("Backend offline");
  expect(onCodex).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Reload agent profiles" }));
  await waitFor(() => expect(screen.getByText("Add an ACP profile to use another agent.")).toBeTruthy());
  fireEvent.click(screen.getByRole("button", { name: "Manage agent profiles" }));
  expect(runtime.setBrowserOpen).toHaveBeenCalledWith(true);
  expect(screen.queryByRole("dialog")).toBeNull();
});

it("closes the chooser on backend change and ignores a late failed creation", async () => {
  let reject: (error: Error) => void = () => {};
  const { open, runtime, node, rerender } = setup({ newConversation: vi.fn(() => new Promise<void>((_, fail) => { reject = fail; })) });
  await open(); fireEvent.click(screen.getByRole("button", { name: "Other agent · ACP" }));
  rerender(node({ ...runtime, backendKey: "daemon" }));
  await act(async () => reject(new Error("old backend disconnected")));
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(screen.queryByRole("alert")).toBeNull();
});

it("supports keyboard cancellation and traps focus in the chooser", async () => {
  const { open } = setup(); await open();
  const first = screen.getByRole("button", { name: "Codex" });
  const last = screen.getByRole("button", { name: "Cancel" });
  last.focus(); fireEvent.keyDown(document, { key: "Tab" }); expect(document.activeElement).toBe(first);
  fireEvent.keyDown(document, { key: "Tab", shiftKey: true }); expect(document.activeElement).toBe(last);
  fireEvent.keyDown(document, { key: "Escape" }); expect(screen.queryByRole("dialog")).toBeNull();
});
