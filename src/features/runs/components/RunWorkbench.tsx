import { useEffect, useMemo, useState } from "react";
import { captureRunInstance, getRunCapture, listRunComparisons, listRunTurns, listWorkspaces, reconcileRun, saveRunComparison } from "@services/tauri";
import { GitDiffViewer } from "@/features/git/components/GitDiffViewer";
import type { RunCapture, RunComparison, RunInstance, RunRecord } from "../types";
import "../runs.css";

type Props = { runs: RunRecord[]; onSelectInstance: (workspaceId: string, threadId: string) => void };
type Tab = "Output" | "Settings" | "Commands" | "Tokens" | "Files";
const pretty = (value: unknown) => JSON.stringify(value, null, 2);

export function RunWorkbench({runs, onSelectInstance}: Props) {
  const options = useMemo(() => runs.flatMap(run => run.instances.flatMap(instance => instance.captures.map(capture => ({run, instance, capture})))), [runs]);
  const [leftId, setLeftId] = useState("");
  const [rightId, setRightId] = useState("");
  const [left, setLeft] = useState<RunCapture | null>(null);
  const [right, setRight] = useState<RunCapture | null>(null);
  const [tab, setTab] = useState<Tab>("Output");
  const [choice, setChoice] = useState<RunComparison["choice"]>("undecided");
  const [notes, setNotes] = useState("");
  const [revision, setRevision] = useState(0);
  const [reload, setReload] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [live, setLive] = useState<Set<string> | null>(null);
  useEffect(() => {
    let current = true;
    listWorkspaces().then(workspaces => {if (current) setLive(new Set(workspaces.map(w => w.id)));}).catch(() => {if (current) setLive(null);});
    return () => {current = false;};
  }, [runs]);
  useEffect(() => {
    let current = true;
    setLeft(null); setRight(null); setError(null); setSaved(false); setNotes(""); setChoice("undecided"); setRevision(0);
    if (!leftId) return;
    setLoading(true);
    Promise.all([getRunCapture(leftId), rightId ? getRunCapture(rightId) : Promise.resolve(null), listRunComparisons({captureId: leftId})])
      .then(([a, b, comparisons]) => {
        if (!current) return;
        setLeft(a); setRight(b);
        const comparison = comparisons.find(c => c.leftCaptureId === leftId && c.rightCaptureId === rightId);
        if (comparison) { setChoice(comparison.choice); setNotes(comparison.notes); setRevision(comparison.revision); }
      }).catch(e => {if (current) setError(String(e));}).finally(() => {if (current) setLoading(false);});
    return () => {current = false;};
  }, [leftId, rightId, reload]);
  const leftOption = options.find(o => o.capture.id === leftId);
  const rightOption = options.find(o => o.capture.id === rightId);
  const select = (side: "Left" | "Right", value: string, change: (id: string) => void) => (
    <label>{side} capture
      <select aria-label={`${side} capture`} value={value} onChange={e => change(e.target.value)} disabled={saving}>
        <option value="">Select a capture</option>
        {options.map(({run, instance, capture}) => <option key={capture.id} value={capture.id}>{run.title} · {instance.requested.modelLabel ?? "Default"} #{instance.ordinal + 1} · {capture.label} · {new Date(capture.createdAt).toLocaleString()}{capture.complete ? "" : " · incomplete"}</option>)}
      </select>
    </label>
  );
  return <section className="run-workbench" aria-label="Run comparison workbench">
    <h2>Run comparison workbench</h2>
    <p>The initial submitted turn is the primary result. Later captures are saved separately.</p>
    {runs.length === 0 && <p>No recorded runs yet.</p>}
    {runs.map(run => <details key={run.id}>
      <summary>{run.title} · {run.instances.length} instances</summary>
      <p>Base: {run.baseOid ?? "unavailable"} · Workspace: {run.workspaceName}</p>
      <button type="button" className="ghost" onClick={async () => {try {const result = await reconcileRun(run.id); setError(result.errors.length ? result.errors.join("; ") : null);} catch (e) {setError(String(e));}}}>Reconcile saved run</button>
      {run.instances.map(instance => <div key={instance.id} className="run-instance-detail">
        <strong>{instance.requested.modelLabel ?? "Default model"} #{instance.ordinal + 1}</strong>
        <p>{instance.preparation} · Submission: {instance.submissionOutcome} · Execution: {instance.executionOutcome}</p>
        <p>Setup: {instance.setup.status}{instance.setup.message ? ` · ${instance.setup.message}` : ""}</p>
        {instance.error && <p role="alert">{instance.error}</p>}
        {instance.workspaceId && live && !live.has(instance.workspaceId) ? <p>Live workspace missing. Saved captures remain available.</p> : instance.threadId && instance.workspaceId ? <button className="ghost" type="button" onClick={() => onSelectInstance(instance.workspaceId!, instance.threadId!)}>Open thread</button> : <p>No recorded thread receipt.</p>}
        {instance.threadId && <LaterCapture instance={instance} onError={setError} />}
      </div>)}
    </details>)}
    <div className="run-comparison-columns">{select("Left", leftId, setLeftId)}{select("Right", rightId, setRightId)}</div>
    {error && <p role="alert">{error}</p>}
    {loading && <p role="status">Loading saved captures…</p>}
    {left && <>
      <div role="tablist" aria-label="Capture evidence">{(["Output", "Settings", "Commands", "Tokens", "Files"] as Tab[]).map(name => <button className="ghost" key={name} type="button" role="tab" aria-selected={tab === name} onClick={() => setTab(name)}>{name}</button>)}</div>
      {right && <p>{leftOption?.run.baseOid === rightOption?.run.baseOid ? "Same recorded base" : "Different recorded bases"} · {leftOption?.run.prompt === rightOption?.run.prompt ? "Same original input" : "Different original inputs"} · {pretty(leftOption?.instance.submission) === pretty(rightOption?.instance.submission) ? "Same submitted payload" : "Submitted payloads differ; inspect Settings"}</p>}
      <div className="run-comparison-columns">
        <Evidence capture={left} instance={leftOption?.instance} run={leftOption?.run} tab={tab} />
        {right && <Evidence capture={right} instance={rightOption?.instance} run={rightOption?.run} tab={tab} />}
      </div>
      {right && left.id !== right.id && <form onSubmit={async event => {
        event.preventDefault(); setSaving(true); setSaved(false); setError(null);
        try { const comparison = await saveRunComparison({leftCaptureId: left.id, rightCaptureId: right.id, choice, notes, expectedRevision: revision}); setRevision(comparison.revision); setSaved(true); }
        catch (e) {setError(String(e));} finally {setSaving(false);}
      }}>
        <label>Human choice <select value={choice} onChange={e => {setChoice(e.target.value as RunComparison["choice"]);setSaved(false);}} disabled={saving}>
          <option value="undecided">Undecided</option><option value="left">Left</option><option value="right">Right</option><option value="tie">Tie</option>
        </select></label>
        <label>Comparison notes<textarea value={notes} onChange={e => {setNotes(e.target.value);setSaved(false);}} disabled={saving} /></label>
        <button className="primary" disabled={saving || loading}>Save comparison</button>
        <button className="ghost" type="button" disabled={saving} onClick={() => setReload(value => value + 1)}>Reload saved notes</button>
        {saved && <span role="status">Comparison saved</span>}
      </form>}
    </>}
  </section>;
}
function LaterCapture({instance, onError}: {instance: RunInstance; onError: (error: string | null) => void}) {
  const [turnId, setTurnId] = useState("");
  const [turns, setTurns] = useState<{id: string; status: string}[]>([]);
  const [busy, setBusy] = useState(false);
  const [request, setRequest] = useState<{id: string; turn: string} | null>(null);
  const [saved, setSaved] = useState(false);
  return <form onSubmit={async e => {
    e.preventDefault(); const turn = turnId.trim(); if (!turn) return;
    const receipt = request?.turn === turn ? request : {id: crypto.randomUUID(), turn}; setRequest(receipt); setBusy(true); onError(null);
    try {await captureRunInstance(instance.id, turn, receipt.id, "Later turn");setSaved(true);setRequest(null);}
    catch (error) {onError(String(error));} finally {setBusy(false);}
  }}>
    <button className="ghost" type="button" disabled={busy} onClick={async () => {
      setBusy(true); onError(null);
      try {const available = await listRunTurns(instance.id); setTurns(available); setTurnId(available.filter(t => t.id !== instance.initialTurnId).slice(-1)[0]?.id ?? "");}
      catch (error) {onError(String(error));} finally {setBusy(false);}
    }}>Choose a later turn</button>
    {turns.length > 0 && <label>Turn to capture<select value={turnId} onChange={e => {setTurnId(e.target.value);setSaved(false);}} disabled={busy}>
      <option value="">Select a turn</option>{turns.map((turn, index) => <option key={turn.id} value={turn.id}>Turn {index + 1} · {turn.status}{turn.id === instance.initialTurnId ? " · initial result" : ""}</option>)}
    </select></label>}
    <button className="ghost" disabled={busy || !turnId.trim()}>Capture later turn</button>
    {saved && <span role="status">Capture saved. History refreshes automatically.</span>}
  </form>;
}
function Evidence({capture, instance, run, tab}: {capture: RunCapture; instance?: RunInstance; run?: RunRecord; tab: Tab}) {
  return <article className="run-evidence" aria-label={`${capture.label} evidence`}>
    <h3>{capture.label}</h3><p>{capture.status} · {capture.complete ? "Complete turn evidence" : "Incomplete evidence"} · {new Date(capture.createdAt).toLocaleString()}</p>
    {capture.errors.map((error, index) => <p key={index}>{error}</p>)}
    {tab === "Output" && (capture.items.filter(i => i.type === "agentMessage").map((i, index) => <pre key={index}>{String(i.text ?? "")}</pre>))}
    {tab === "Settings" && <><h4>Original input</h4><pre>{run?.prompt}</pre><h4>Requested settings</h4><pre>{pretty(instance?.requested)}</pre><h4>Submitted payload</h4><pre>{pretty(instance?.submission)}</pre><h4>Observed runtime metadata</h4><pre>{pretty(capture.observed)}</pre></>}
    {tab === "Commands" && <><p>Command evidence comes from recorded tool items. Assistant claims are displayed under Output.</p>{capture.items.filter(i => i.type === "commandExecution").map((i, index) => <pre key={index}>{pretty({sourceItemId: i.id, command: i.command, exitCode: i.exitCode ?? "unavailable", status: i.status, output: i.aggregatedOutput ?? "unavailable"})}</pre>)}</>}
    {tab === "Tokens" && <pre>{capture.usage ? pretty(capture.usage) : "Turn-attributable token usage unavailable"}</pre>}
    {tab === "Files" && <><h4>Turn-reported changes</h4><pre>{pretty(capture.items.filter(i => i.type === "fileChange"))}</pre><h4>Workspace state at capture</h4><p>May include setup, user, or other-process edits. This is not an atomic snapshot.</p><p>Base: {capture.git.baseOid ?? "unavailable"} · HEAD: {capture.git.headOid ?? "unavailable"}</p>
      {capture.git.concurrentChangeDetected && <p>Concurrent Git change detected</p>}
      <details><summary>Pre-submission baseline</summary><pre>{pretty(instance?.baseline)}</pre></details>
      {capture.git.files?.filter(f => f.omission).map(f => <p key={f.path}>{f.path}: {f.omission}</p>)}
      <GitDiffViewer diffs={capture.git.files ?? []} selectedPath={null} isLoading={false} error={capture.git.errors?.join("; ") || null} canRevert={false} ignoreWhitespaceChanges={false} />
    </>}
  </article>;
}
