import { useState } from "react";
import type { ApprovalDecision, ApprovalRequest } from "@/types";
import { openUrl } from "@tauri-apps/plugin-opener";
import { ToastActions, ToastError } from "@/features/design-system/components/toast/ToastPrimitives";

type Props = {
  request: ApprovalRequest;
  onDecision: (request: ApprovalRequest, decision: ApprovalDecision) => void | Promise<void>;
};

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

export function PermissionRequestForm({ request, onDecision }: Props) {
  const permissions = record(request.params.permissions);
  const fileSystem = record(permissions.fileSystem);
  const choices: { key: string; label: string; group: string; value: unknown }[] = [];
  if (record(permissions.network).enabled === true) {
    choices.push({ key: "network", label: "Network access", group: "network", value: true });
  }
  for (const group of ["read", "write", "entries"]) {
    const entries = fileSystem[group];
    if (!Array.isArray(entries)) continue;
    entries.forEach((value, index) => choices.push({
      key: `${group}-${index}`, group, value,
      label: group === "entries" ? `${record(value).access}: ${JSON.stringify(record(value).path)}` : `${group}: ${value}`,
    }));
  }
  const [selected, setSelected] = useState(() => new Set(choices.map((choice) => choice.key)));
  const [scope, setScope] = useState<"turn" | "session">("turn");
  const [error, setError] = useState("");
  const [pending, setPending] = useState(false);
  const submit = async (accept: boolean) => {
    const granted: Record<string, unknown> = {};
    const fs: Record<string, unknown> = {};
    if (accept) for (const choice of choices) {
      // Deny entries constrain grants and must never be dropped by deselection.
      if (!selected.has(choice.key) && record(choice.value).access !== "deny") continue;
      if (choice.group === "network") granted.network = { enabled: true };
      else { const entries = (fs[choice.group] ??= []) as unknown[]; entries.push(choice.value); }
    }
    if (Object.keys(fs).length) {
      if (fileSystem.globScanMaxDepth != null) fs.globScanMaxDepth = fileSystem.globScanMaxDepth;
      granted.fileSystem = fs;
    }
    setPending(true);
    try { await onDecision(request, { permissions: granted, scope }); }
    catch (e) { setError(String(e)); }
    finally { setPending(false); }
  };
  return <div>
    <p>{String(request.params.reason ?? "Select the permissions to grant.")}</p>
    {choices.map((choice) => <label key={choice.key} style={{ display: "block" }}>
      <input type="checkbox" checked={selected.has(choice.key)} disabled={pending || record(choice.value).access === "deny"}
        onChange={(event) => setSelected((previous) => { const next = new Set(previous); if (event.target.checked) next.add(choice.key); else next.delete(choice.key); return next; })} />
      {choice.label}
    </label>)}
    <label>Grant duration <select value={scope} disabled={pending} onChange={(event) => setScope(event.target.value as "turn" | "session")}>
      <option value="turn">This turn</option><option value="session">This session</option>
    </select></label>
    {error && <ToastError>{error}</ToastError>}
    <ToastActions><button className="secondary" disabled={pending} onClick={() => void submit(false)}>Decline</button>
      <button className="primary" disabled={pending || choices.length === 0} onClick={() => void submit(true)}>Grant selected permissions</button></ToastActions>
  </div>;
}

export function ElicitationRequestForm({ request, onDecision }: Props) {
  const params = request.params;
  const schema = record(params.requestedSchema);
  const properties = record(schema.properties);
  const required = Array.isArray(schema.required) ? schema.required : [];
  const [values, setValues] = useState<Record<string, unknown>>(() => Object.fromEntries(
    Object.entries(properties).filter(([, value]) => record(value).default != null).map(([key, value]) => [key, record(value).default]),
  ));
  const [error, setError] = useState("");
  const [pending, setPending] = useState(false);
  const formMode = params.mode === "form";
  const url = typeof params.url === "string" && /^https?:\/\//i.test(params.url) ? params.url : null;
  const supported = formMode || (params.mode === "url" && url !== null);
  const submit = async (action: "accept" | "decline" | "cancel") => {
    setPending(true); setError("");
    try { await onDecision(request, { action, ...(action === "accept" && formMode ? { content: values } : {}) }); }
    catch (e) { setError(String(e)); }
    finally { setPending(false); }
  };
  return <form onSubmit={(event) => { event.preventDefault(); void submit("accept"); }}>
    <p>{String(params.message ?? params.description ?? "Input requested")}</p>
    {!supported && <ToastError>This input mode requires a client with {String(params.mode)} support. You can decline or cancel this request.</ToastError>}
    {formMode && Object.entries(properties).map(([key, raw]) => {
      const field = record(raw);
      const options = (Array.isArray(field.enum) ? field.enum.map((value, index) => ({ value: String(value), label: String((field.enumNames as unknown[] | undefined)?.[index] ?? value) }))
        : Array.isArray(field.oneOf) ? field.oneOf.map((value) => ({ value: String(record(value).const), label: String(record(value).title ?? record(value).const) })) : []);
      const set = (value: unknown) => setValues((previous) => ({ ...previous, [key]: value }));
      const title = String(field.title ?? key);
      return <label key={key} style={{ display: "block" }}>{title}{required.includes(key) ? " *" : ""}
        {field.description ? <small>{String(field.description)}</small> : null}
        {field.type === "boolean" ? <select aria-label={title} required={required.includes(key)} disabled={pending} value={values[key] == null ? "" : String(values[key])} onChange={(event) => set(event.target.value === "" ? undefined : event.target.value === "true")}>
          <option value="">Choose…</option><option value="true">Yes</option><option value="false">No</option>
        </select> : field.type === "array" ? <select multiple aria-label={title} required={required.includes(key)} disabled={pending} value={Array.isArray(values[key]) ? values[key] as string[] : []}
          onChange={(event) => set(Array.from(event.target.selectedOptions, (option) => option.value))}>
          {(Array.isArray(record(field.items).enum) ? record(field.items).enum as unknown[] : Array.isArray(record(field.items).anyOf) ? record(field.items).anyOf as unknown[] : []).map((option) => {
            const value = typeof option === "string" ? option : String(record(option).const);
            return <option key={value} value={value}>{typeof option === "string" ? option : String(record(option).title ?? value)}</option>;
          })}
        </select> : options.length ? <select aria-label={title} required={required.includes(key)} disabled={pending} value={String(values[key] ?? "")} onChange={(event) => set(event.target.value)}>
          <option value="">Choose…</option>{options.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}
        </select> : <input aria-label={title} required={required.includes(key)} disabled={pending}
          type={field.type === "number" || field.type === "integer" ? "number" : field.format === "email" ? "email" : field.format === "uri" ? "url" : field.format === "date" ? "date" : "text"}
          min={typeof field.minimum === "number" ? field.minimum : undefined} max={typeof field.maximum === "number" ? field.maximum : undefined}
          minLength={typeof field.minLength === "number" ? field.minLength : undefined} maxLength={typeof field.maxLength === "number" ? field.maxLength : undefined}
          step={field.type === "integer" ? 1 : "any"} value={String(values[key] ?? "")}
          onChange={(event) => set(event.target.value === "" ? undefined : field.type === "number" || field.type === "integer" ? Number(event.target.value) : event.target.value)} />}
      </label>;
    })}
    {params.mode === "url" && url && <button type="button" className="secondary" onClick={() => void openUrl(url).catch((e) => setError(String(e)))}>Open {url}</button>}
    {error && <ToastError>{error}</ToastError>}
    <ToastActions>
      <button type="button" disabled={pending} className="secondary" onClick={() => void submit("cancel")}>Cancel</button>
      <button type="button" disabled={pending} className="secondary" onClick={() => void submit("decline")}>Decline</button>
      <button type="submit" disabled={pending || !supported} className="primary">{formMode ? "Submit" : "Completed"}</button>
    </ToastActions>
  </form>;
}
