// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { PermissionRequestForm, ElicitationRequestForm } from "./ServerRequestForm";
import type { ApprovalRequest } from "@/types";

vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));
afterEach(cleanup);
const base = { workspace_id: "ws", request_id: 12 };

describe("current app-server requests", () => {
  it("grants only selected permissions with explicit scope", async () => {
    const request: ApprovalRequest = { ...base, method: "item/permissions/requestApproval", params: {
      permissions: { network: { enabled: true }, fileSystem: { read: ["/one", "/two"] } },
    } };
    const onDecision = vi.fn();
    render(<PermissionRequestForm request={request} onDecision={onDecision} />);
    fireEvent.click(screen.getByLabelText("Network access"));
    fireEvent.click(screen.getByLabelText("read: /two"));
    fireEvent.click(screen.getByText("Grant selected permissions"));
    await waitFor(() => expect(onDecision).toHaveBeenCalledWith(request, { permissions: { fileSystem: { read: ["/one"] } }, scope: "turn" }));
  });

  it("declines permissions with an empty grant", async () => {
    const request: ApprovalRequest = { ...base, method: "item/permissions/requestApproval", params: { permissions: { network: { enabled: true } } } };
    const onDecision = vi.fn();
    render(<PermissionRequestForm request={request} onDecision={onDecision} />);
    fireEvent.click(screen.getByText("Decline"));
    await waitFor(() => expect(onDecision).toHaveBeenCalledWith(request, { permissions: {}, scope: "turn" }));
  });

  it("submits typed MCP form values and retains failed requests", async () => {
    const request: ApprovalRequest = { ...base, method: "mcpServer/elicitation/request", params: {
      mode: "form", message: "Choose settings", requestedSchema: { type: "object", required: ["count", "enabled"], properties: {
        count: { type: "integer", minimum: 1 }, enabled: { type: "boolean" },
      } },
    } };
    const onDecision = vi.fn().mockRejectedValue(new Error("Disconnected"));
    render(<ElicitationRequestForm request={request} onDecision={onDecision} />);
    fireEvent.change(screen.getByLabelText("count"), { target: { value: "3" } });
    fireEvent.change(screen.getByLabelText("enabled"), { target: { value: "false" } });
    fireEvent.click(screen.getByText("Submit"));
    await waitFor(() => expect(onDecision).toHaveBeenCalledWith(request, { action: "accept", content: { count: 3, enabled: false } }));
    expect(await screen.findByText("Error: Disconnected")).toBeTruthy();
    expect(screen.getByText("Submit").hasAttribute("disabled")).toBe(false);
  });

  it("does not accept an unsupported verification mode", () => {
    const request: ApprovalRequest = { ...base, method: "mcpServer/elicitation/request", params: { mode: "openai/userVerification" } };
    render(<ElicitationRequestForm request={request} onDecision={vi.fn()} />);
    expect(screen.getByText("Completed").hasAttribute("disabled")).toBe(true);
    expect(screen.getByText("Decline").hasAttribute("disabled")).toBe(false);
  });
});
