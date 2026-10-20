import { ChevronRight, Settings, TerminalSquare } from "lucide-react";
import { useCallback, useEffect, useRef, useState, type ComponentProps } from "react";
import { createPortal } from "react-dom";
import { Sidebar } from "@app/components/Sidebar";
import { ModalShell } from "@/features/design-system/components/modal/ModalShell";
import type { WorkspaceInfo } from "@/types";
import type { AcpConversations } from "../hooks/useAcpConversations";
import { runtimeError } from "../types";
import { AgentSessionBrowser } from "./AgentSessionBrowser";

type Props = {
  sidebarProps: ComponentProps<typeof Sidebar>;
  runtime: AcpConversations;
};

// The parent keys this surface by backend, including any pending creation dialog.
export function RuntimeSidebar({ sidebarProps, runtime }: Props) {
  const [workspace, setWorkspace] = useState<WorkspaceInfo | null>(null);
  return <>
    <Sidebar
      {...sidebarProps}
      onAddAgent={setWorkspace}
      agentSessionBrowserNode={<AgentSessionBrowser runtime={runtime} />}
    />
    {workspace && <NewAgentDialog
      workspace={workspace}
      runtime={runtime}
      onCodex={() => sidebarProps.onAddAgent(workspace)}
      onClose={() => setWorkspace(null)}
    />}
  </>;
}

function NewAgentDialog({ workspace, runtime, onCodex, onClose }: {
  workspace: WorkspaceInfo;
  runtime: AcpConversations;
  onCodex: () => void;
  onClose: () => void;
}) {
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [creating, setCreating] = useState<string | null>(null);
  const card = useRef<HTMLDivElement>(null);
  const pending = useRef(false);
  const mounted = useRef(true);
  const refreshVersion = useRef(0);
  const close = useRef(onClose);
  close.current = onClose;

  const refreshCatalog = runtime.refreshCatalog;
  const refresh = useCallback(async () => {
    const version = ++refreshVersion.current;
    setLoading(true);
    setError(null);
    try { await refreshCatalog(); }
    catch (e) { if (mounted.current && version === refreshVersion.current) setError(runtimeError(e)); }
    finally { if (mounted.current && version === refreshVersion.current) setLoading(false); }
  }, [refreshCatalog]);
  useEffect(() => {
    mounted.current = true;
    void refresh();
    return () => { mounted.current = false; };
    // Load the current backend's profiles when opening, independently of selection.
  }, [refresh]);
  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null;
    card.current?.focus();
    const keydown = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !pending.current) close.current();
      if (event.key !== "Tab") return;
      const buttons = Array.from(card.current?.querySelectorAll<HTMLButtonElement>("button:not(:disabled)") ?? []);
      if (!buttons.length) { event.preventDefault(); card.current?.focus(); return; }
      const first = buttons[0], last = buttons[buttons.length - 1];
      if (event.shiftKey && (document.activeElement === first || document.activeElement === card.current)) {
        event.preventDefault(); last?.focus();
      } else if (!event.shiftKey && (document.activeElement === last || document.activeElement === card.current)) {
        event.preventDefault(); first?.focus();
      }
    };
    document.addEventListener("keydown", keydown);
    return () => { document.removeEventListener("keydown", keydown); previous?.focus(); };
  }, []);

  const create = async (profileId: string | null) => {
    if (pending.current) return;
    pending.current = true;
    setCreating(profileId ?? "codex");
    setError(null);
    try {
      if (profileId === null) onCodex();
      else await runtime.newConversation(workspace.id, profileId);
      if (mounted.current) onClose();
    } catch (e) {
      if (mounted.current) setError(runtimeError(e));
    } finally {
      pending.current = false;
      if (mounted.current) setCreating(null);
    }
  };
  return createPortal(<ModalShell ariaLabel="New agent" onBackdropClick={() => { if (!pending.current) onClose(); }}>
    <div ref={card} tabIndex={-1} className="agent-creation-dialog">
      <h2>New agent</h2>
      <p>Choose an agent for {workspace.name}.</p>
      <p className="agent-creation-directory">{workspace.path}</p>
      <button className="ghost agent-creation-choice" type="button" disabled={!!creating} onClick={() => void create(null)}><TerminalSquare aria-hidden="true" /><span>Codex</span><ChevronRight aria-hidden="true" /></button>
      {runtime.profiles.filter(profile => profile.kind !== "codex").map(profile => <button
        className="ghost agent-creation-choice" type="button" key={profile.id} disabled={loading || !!creating}
        onClick={() => void create(profile.id)}
      ><TerminalSquare aria-hidden="true" /><span>{profile.name} · ACP</span><ChevronRight aria-hidden="true" /></button>)}
      {loading && <p role="status">Loading agent profiles…</p>}
      {!loading && !error && runtime.profiles.every(profile => profile.kind === "codex") && <p>Add an ACP profile to use another agent.</p>}
      {creating && <p role="status">Creating agent…</p>}
      {error && <><p role="alert">{error}</p><button className="ghost" type="button" disabled={loading || !!creating} onClick={() => void refresh()}>Reload agent profiles</button></>}
      <div className="agent-creation-footer"><button className="ghost" type="button" disabled={!!creating} onClick={() => { onClose(); runtime.setBrowserOpen(true); }}><Settings size={16} aria-hidden="true" />Manage agent profiles</button>
      <button className="ghost" type="button" disabled={!!creating} onClick={onClose}>Cancel</button></div>
    </div>
  </ModalShell>, document.body);
}
