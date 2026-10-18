// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { ChatPane } from "@/features/layout/components/ChatPane";
import layoutCss from "@/styles/main.css?raw";
import runtimeCss from "@/styles/agent-conversations.css?raw";
import type { AcpConversations } from "../hooks/useAcpConversations";
import { AcpConversationControls } from "./AcpConversationControls";

afterEach(() => { cleanup(); document.head.querySelectorAll("style[data-controls-test]").forEach(node => node.remove()); vi.restoreAllMocks(); });

function setup(overrides: Partial<AcpConversations> = {}) {
  const style = document.createElement("style");
  style.dataset.controlsTest = "true";
  style.textContent = layoutCss + runtimeCss;
  document.head.append(style);
  const runtime = {
    session: {
      sessionId: "session", profile: { name: "Independent ACP" },
      capabilities: { loadSession: true, images: true, audio: true, embeddedContext: true },
      configuration: { configOptions: [
        { id: "model", name: "Model", type: "select", currentValue: "first", options: [{ group: "models", name: "Models", options: [{ value: "first", name: "First" }, { value: "second", name: "Second" }] }] },
        { id: "effort", name: "Effort", type: "select", currentValue: "low", options: [{ value: "low", name: "Low" }, { value: "high", name: "High" }] },
        { id: "mode", name: "Session Mode", type: "select", currentValue: "build", options: [{ value: "build", name: "Build" }, { value: "plan", name: "Plan" }] },
      ] },
    },
    page: { coverage: "agent_replay", freshness: { stale: false }, readOnly: false },
    permissions: {}, queue: [], content: [], loading: false, sending: false,
    configure: vi.fn().mockResolvedValue(undefined), refresh: vi.fn().mockResolvedValue(undefined),
    addContent: vi.fn(), clearContent: vi.fn(), ...overrides,
  } as unknown as AcpConversations;
  render(<ChatPane messagesNode={<p>Conversation</p>} composerNode={<><AcpConversationControls runtime={runtime} /><div className="composer">Composer</div></>} />);
  return runtime;
}

it("restores mouse hit testing for every ACP control inside the real composer overlay", () => {
  setup();
  const model = screen.getByRole("combobox", { name: "Model" });
  const overlay = model.closest(".chat-pane-composer")!;
  expect(getComputedStyle(overlay).pointerEvents).toBe("none");
  for (const node of [...screen.getAllByRole("combobox"), ...screen.getAllByRole("button")]) {
    expect(getComputedStyle(node).pointerEvents).toBe("auto");
  }
});

it.each([["Model", "model", "second"], ["Effort", "effort", "high"], ["Session Mode", "mode", "plan"]])("routes the advertised %s selector to ACP configuration", (name, id, value) => {
  const runtime = setup();
  fireEvent.change(screen.getByRole("combobox", { name }), { target: { value } });
  expect(runtime.configure).toHaveBeenCalledExactlyOnceWith(id, value);
  expect(runtime.addContent).not.toHaveBeenCalled();
});

it("keeps attachment selection optional and hidden until explicitly requested", () => {
  const runtime = setup();
  const input = screen.getByLabelText("Optional agent attachments") as HTMLInputElement;
  expect(input.hidden).toBe(true);
  expect(input.required).toBe(false);
  expect(runtime.addContent).not.toHaveBeenCalled();
  const pick = vi.spyOn(input, "click").mockImplementation(() => {});
  fireEvent.click(screen.getByRole("button", { name: "Attach files (optional)" }));
  expect(pick).toHaveBeenCalledOnce();
  expect(runtime.addContent).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Refresh conversation" }));
  expect(runtime.refresh).toHaveBeenCalledOnce();
});

it("adds context only after the user explicitly selects a file", async () => {
  const runtime = setup();
  fireEvent.change(screen.getByLabelText("Optional agent attachments"), { target: { files: [new File(["Optional context"], "notes.txt", { type: "text/plain" })] } });
  await waitFor(() => expect(runtime.addContent).toHaveBeenCalledExactlyOnceWith({ type: "resource", resource: { uri: "attachment://notes.txt", mimeType: "text/plain", blob: btoa("Optional context") } }));
});

it("disables controls for read-only ownership", () => {
  setup({ page: { coverage: "agent_replay", freshness: { stale: false }, readOnly: true } as AcpConversations["page"] });
  for (const node of [...screen.getAllByRole("combobox"), ...screen.getAllByRole("button")]) expect((node as HTMLButtonElement).disabled).toBe(true);
});
