import { useRef } from "react";
import { Paperclip, RefreshCw } from "lucide-react";
import type { AcpConversations } from "../hooks/useAcpConversations";
import type { RuntimeConfigOption } from "../types";

export function AcpConversationControls({ runtime: r }: { runtime: AcpConversations }) {
  const attachments = useRef<HTMLInputElement>(null);
  const session = r.session;
  const options: RuntimeConfigOption[] = session?.configuration?.configOptions ?? [
    ...(session?.configuration?.modes ? [{ id: "mode", name: "Mode", type: "select", currentValue: session.configuration.modes.currentModeId, options: session.configuration.modes.availableModes.map(m => ({ value: m.id, name: m.name })) }] : []),
    ...(session?.configuration?.models ? [{ id: "model", name: "Model", type: "select", currentValue: session.configuration.models.currentModelId, options: session.configuration.models.availableModels.map(m => ({ value: m.modelId, name: m.name })) }] : []),
  ];
  const supportsAttachments = session?.capabilities.images || session?.capabilities.audio || session?.capabilities.embeddedContext;

  return <div className="acp-conversation-controls">
    <div className="acp-conversation-status">
      <p><span className="acp-conversation-agent">{session?.profile.name ?? "ACP conversation"}</span> · {r.page?.coverage === "agent_replay" ? "Agent-supplied conversation" : "Observed history only"}{r.loading ? " · Refreshing history…" : ""}{r.page?.freshness.stale ? " · Cached history is stale" : ""}{r.page?.readOnly ? " · Read-only: another backend owns this session" : ""}</p>
      <button type="button" className="ghost acp-conversation-refresh" disabled={r.loading || !!r.active || !!r.pending || r.sending || !!r.queue.length || !!Object.keys(r.permissions).length || r.page?.readOnly} onClick={() => void r.refresh()}>
        <RefreshCw size={14} aria-hidden="true" />Refresh conversation
      </button>
    </div>
    {session && !session.capabilities.loadSession && <p>Full history loading is unavailable for this agent.{r.page?.connectionReady === false ? " Create a new conversation to continue with this agent." : ""}</p>}
    <div className="acp-conversation-options">
      {options.filter(o => o.type === "select").map(o => <label key={o.id} className="acp-conversation-option">
        <span>{o.name}</span>
        <select value={o.currentValue} disabled={r.loading || !!r.active || r.sending || r.page?.readOnly} onChange={e => void r.configure(o.id, e.target.value)}>
          {o.options.map(c => "value" in c ? <option key={c.value} value={c.value}>{c.name}</option> : <optgroup key={c.name} label={c.name}>{c.options.map(v => <option key={v.value} value={v.value}>{v.name}</option>)}</optgroup>)}
        </select>
      </label>)}
      {supportsAttachments && <>
        <button type="button" className="ghost acp-conversation-attach" disabled={r.loading || r.page?.readOnly} title="Optionally include files with your next message. No attachments are required." onClick={() => attachments.current?.click()}>
          <Paperclip size={14} aria-hidden="true" />Attach files (optional)
        </button>
        <input ref={attachments} hidden aria-label="Optional agent attachments" id="acp-attachments" type="file" multiple disabled={r.loading || r.page?.readOnly} accept={`${session?.capabilities.images ? "image/*," : ""}${session?.capabilities.audio ? "audio/*," : ""}${session?.capabilities.embeddedContext ? "text/*,application/json,application/pdf" : ""}`} onChange={async e => {
          for (const file of Array.from(e.target.files ?? [])) {
            const data = await new Promise<string>((resolve, reject) => { const reader = new FileReader(); reader.onload = () => resolve(String(reader.result).split(",")[1]); reader.onerror = () => reject(reader.error); reader.readAsDataURL(file); });
            r.addContent(file.type.startsWith("image/") ? { type: "image", mimeType: file.type, data } : file.type.startsWith("audio/") ? { type: "audio", mimeType: file.type, data } : { type: "resource", resource: { uri: `attachment://${encodeURIComponent(file.name)}`, mimeType: file.type || "application/octet-stream", blob: data } });
          }
          e.target.value = "";
        }} />
      </>}
    </div>
    {Object.entries(r.permissions).map(([id, p]) => <fieldset key={id} disabled={r.page?.readOnly}><legend>Agent permission request</legend><pre>{JSON.stringify(p.toolCall ?? p, null, 2)}</pre>{(p.options as { optionId: string; name: string }[] | undefined)?.map(o => <button key={o.optionId} type="button" className="ghost" onClick={() => void r.permission(id, o.optionId)}>{o.name}</button>)}</fieldset>)}
    {!!r.content.length && <p>{r.content.length} attached content blocks <button type="button" className="ghost" onClick={r.clearContent}>Clear attachments</button></p>}
    {r.error && <p role="alert">{r.error}</p>}
    {r.uncertain && <><p>Submission outcome is uncertain. The prompt will not be resent automatically.</p><button type="button" className="ghost" onClick={() => void r.checkReceipt()}>Check submission receipt</button></>}
    {!!r.queue.length && <p role="status">{r.queue.length} queued messages</p>}
  </div>;
}
