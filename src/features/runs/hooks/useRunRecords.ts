import { useCallback, useEffect, useRef, useState } from "react";
import { listRuns, reconcileRun } from "@services/tauri";
import type { RunRecord } from "../types";

export function useRunRecords(workspaceId: string | null, backendKey: string, visible = true) {
  const [state, setState] = useState<{key: string; storeId: string | null; runs: RunRecord[]; error: string | null; loading: boolean}>({key: "", storeId: null, runs: [], error: null, loading: true});
  const key = `${backendKey}:${workspaceId ?? ""}`;
  const currentKey = useRef(key);
  currentKey.current = key;
  const generation = useRef(0);
  const loading = useRef(false);
  const refresh = useCallback(async () => {
    if (!workspaceId || loading.current || currentKey.current !== key) return;
    const version = generation.current;
    loading.current = true;
    try {
      let page = await listRuns(workspaceId);
      const records = [...page.runs];
      while (page.nextCursor) {
        page = await listRuns(workspaceId, page.nextCursor);
        if (page.storeId !== records[0]?.storeId && records.length) throw new Error("Run store changed while loading; refresh history");
        records.push(...page.runs);
      }
      if (version !== generation.current) return;
      setState({key, storeId: page.storeId, runs: records, error: null, loading: false});
      // Restore event subscriptions after a backend restart; never relaunch.
      const recover = records.filter(run => run.instances.some(i => (i.threadId || i.sessionId) && (i.executionOutcome === "running" || i.executionOutcome === "unknown" || ((i.initialTurnId || i.initialExecutionId) && !i.captures.length))));
      for (const run of recover) {
        const result = await reconcileRun(run.id);
        if (version !== generation.current) return;
        setState(previous => ({...previous, runs: previous.runs.map(r => r.id === run.id ? result.run : r), error: result.errors.length ? result.errors.join("; ") : previous.error}));
      }
    } catch (error) {
      if (version === generation.current) setState(previous => ({...previous, key, error: String(error), loading: false}));
    } finally {
      if (version === generation.current) loading.current = false;
    }
  }, [workspaceId, key]);
  useEffect(() => {
    generation.current += 1;
    loading.current = false;
    setState({key, storeId: null, runs: [], error: null, loading: true});
    if (visible) void refresh();
    const tick = () => { if (visible && document.visibilityState !== "hidden") void refresh(); };
    const timer = window.setInterval(tick, 5000);
    window.addEventListener("focus", tick);
    window.addEventListener("online", tick);
    return () => { generation.current += 1; loading.current = false; window.clearInterval(timer); window.removeEventListener("focus", tick); window.removeEventListener("online", tick); };
  }, [key, refresh, visible]);
  return {...(state.key === key ? state : {key, storeId: null, runs: [], error: null, loading: true}), refresh};
}
