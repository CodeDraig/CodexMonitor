import type { RuntimeProfile, RuntimeSession } from "@/features/runtime/types";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type {
  ModelOption,
  SendMessageResult,
  ServiceTier,
  WorkspaceInfo,
} from "../../../types";
import { generateRunMetadata, runtimeRequest, buildRuntimePrompt, createRun, recordRunFailure, recordRunSetup, getRun, updateRunMetadata } from "@services/tauri";
import type { RunContext, RunRecord, SetupObservation } from "@/features/runs/types";
import { useRunRecords } from "@/features/runs/hooks/useRunRecords";

export type WorkspaceRunMode = "local" | "worktree";

export type WorkspaceHomeRunInstance = {
  id: string;
  workspaceId: string;
  threadId: string;
  sessionId?: string;
  modelId: string | null;
  modelLabel: string;
  sequence: number;
  durableStatus?: string;
};

export type WorkspaceHomeRun = {
  record?: RunRecord;
  id: string;
  workspaceId: string;
  title: string;
  prompt: string;
  createdAt: number;
  mode: WorkspaceRunMode;
  instances: WorkspaceHomeRunInstance[];
  status: "pending" | "ready" | "partial" | "failed" | "unknown";
  error: string | null;
  instanceErrors: Array<{ message: string }>;
};

type UseWorkspaceHomeOptions = {
  backendKey?: string;
  visible?: boolean;
  activeWorkspace: WorkspaceInfo | null;
  models: ModelOption[];
  selectedModelId: string | null;
  effort?: string | null;
  serviceTier?: ServiceTier | null | undefined;
  collaborationMode?: Record<string, unknown> | null;
  seedThreadCodexParams?: (
    workspaceId: string,
    threadId: string,
    patch: {
      modelId: string | null;
      effort: string | null;
      serviceTier: ServiceTier | null | undefined;
    },
  ) => void;
  addWorktreeAgent: (
    workspace: WorkspaceInfo,
    branch: string,
    options?: { activate?: boolean; runContext?: RunContext },
  ) => Promise<WorkspaceInfo | null>;
  connectWorkspace: (workspace: WorkspaceInfo) => Promise<void>;
  startThreadForWorkspace: (
    workspaceId: string,
    options?: { activate?: boolean; runContext?: RunContext },
  ) => Promise<string | null>;
  sendUserMessageToThread: (
    workspace: WorkspaceInfo,
    threadId: string,
    text: string,
    images?: string[],
    options?: {
      runContext?: RunContext;
      model?: string | null;
      effort?: string | null;
      serviceTier?: ServiceTier | null | undefined;
      collaborationMode?: Record<string, unknown> | null;
    },
  ) => Promise<void | SendMessageResult>;
  onWorktreeCreated?: (worktree: WorkspaceInfo, parent: WorkspaceInfo) => Promise<SetupObservation | void> | void;
};

type WorkspaceHomeState = {
  draftsByWorkspace: Record<string, string>;
  modeByWorkspace: Record<string, WorkspaceRunMode>;
  modelSelectionsByWorkspace: Record<string, Record<string, number>>;
  errorByWorkspace: Record<string, string | null>;
  submittingByWorkspace: Record<string, boolean>;
};

const DEFAULT_MODE: WorkspaceRunMode = "local";
const EMPTY_SELECTIONS: Record<string, number> = {};
const MAX_TITLE_LENGTH = 56;

const buildRunTitle = (prompt: string) => {
  const firstLine = prompt.trim().split("\n")[0] ?? "";
  const normalized = firstLine.replace(/\s+/g, " ").trim();
  if (!normalized) {
    return "New run";
  }
  if (normalized.length > MAX_TITLE_LENGTH) {
    return `${normalized.slice(0, MAX_TITLE_LENGTH)}...`;
  }
  return normalized;
};

const ALLOWED_PREFIXES = [
  "feat",
  "fix",
  "chore",
  "test",
  "docs",
  "refactor",
  "perf",
  "build",
  "ci",
  "style",
];

const PREFIX_RULES: Array<{ prefix: string; keywords: string[] }> = [
  { prefix: "test", keywords: ["test", "tests", "testing"] },
  { prefix: "docs", keywords: ["doc", "docs", "documentation", "readme"] },
  { prefix: "chore", keywords: ["chore", "cleanup", "maintenance"] },
  { prefix: "refactor", keywords: ["refactor"] },
  { prefix: "perf", keywords: ["perf", "performance", "optimize", "optimization"] },
  { prefix: "build", keywords: ["build", "bundle", "compile"] },
  { prefix: "ci", keywords: ["ci", "pipeline", "workflow"] },
  { prefix: "style", keywords: ["style", "format", "lint"] },
  {
    prefix: "fix",
    keywords: [
      "fix",
      "bug",
      "error",
      "issue",
      "broken",
      "regression",
      "crash",
      "failure",
    ],
  },
];

const resolveWorktreePrefix = (prompt: string) => {
  const lower = prompt.toLowerCase();
  const matched = PREFIX_RULES.find((rule) =>
    rule.keywords.some((keyword) => lower.includes(keyword)),
  );
  return matched?.prefix ?? "feat";
};

const buildWorktreeBranch = (prompt: string) => {
  const prefix = resolveWorktreePrefix(prompt);
  const base = prompt
    .toLowerCase()
    .replace(/[^a-z0-9\s-]/g, " ")
    .trim()
    .split(/\s+/)
    .slice(0, 4)
    .join("-");
  const slug = base || `run-${Math.random().toString(36).slice(2, 6)}`;
  return `${prefix}/${slug}`;
};

const resolveModelLabel = (model: ModelOption | null, fallback: string) =>
  model?.displayName?.trim() || model?.model?.trim() || fallback;

const normalizeWorktreeName = (value: string | null | undefined) => {
  if (!value) {
    return null;
  }
  const trimmed = value.trim().toLowerCase();
  for (const prefix of ALLOWED_PREFIXES) {
    const prefixWithSlash = `${prefix}/`;
    if (trimmed.startsWith(prefixWithSlash)) {
      const remainder = trimmed.slice(prefixWithSlash.length).replace(/^\/+/, "");
      return remainder ? `${prefixWithSlash}${remainder}` : null;
    }
  }
  for (const prefix of ALLOWED_PREFIXES) {
    const dashPrefix = `${prefix}-`;
    if (trimmed.startsWith(dashPrefix)) {
      const remainder = trimmed.slice(dashPrefix.length).replace(/^\/+/, "");
      return remainder ? `${prefix}/${remainder}` : null;
    }
  }
  const fallback = trimmed.replace(/^\/+/, "");
  return fallback ? `feat/${fallback}` : null;
};

export function useWorkspaceHome({
  activeWorkspace,
  backendKey = "local",
  visible = true,
  models,
  selectedModelId,
  effort = null,
  serviceTier = undefined,
  collaborationMode = null,
  seedThreadCodexParams,
  addWorktreeAgent,
  connectWorkspace,
  startThreadForWorkspace,
  sendUserMessageToThread,
  onWorktreeCreated,
}: UseWorkspaceHomeOptions) {
  const [state, setState] = useState<WorkspaceHomeState>({
    draftsByWorkspace: {},
    modeByWorkspace: {},
    modelSelectionsByWorkspace: {},
    errorByWorkspace: {},
    submittingByWorkspace: {},
  });

  const [openedRuntimeSessionId, setOpenedRuntimeSessionId] = useState<string | null>(null);
  const [runtimeSelection, setRuntimeSelection] = useState<RuntimeProfile[]>([]);
  useEffect(() => { setRuntimeSelection([]); setOpenedRuntimeSessionId(null); }, [backendKey, activeWorkspace?.id]);
  const launching = useRef(false);
  const pendingCreation = useRef<{key: string; id: string} | null>(null);
  const activeWorkspaceId = activeWorkspace?.id ?? null;
  const records = useRunRecords(activeWorkspaceId, backendKey, visible);
  const runs = useMemo(() => records.runs.map(toWorkspaceHomeRun), [records.runs]);
  const draft = activeWorkspaceId ? state.draftsByWorkspace[activeWorkspaceId] ?? "" : "";
  const runMode = activeWorkspaceId
    ? state.modeByWorkspace[activeWorkspaceId] ?? DEFAULT_MODE
    : DEFAULT_MODE;
  const modelSelections = useMemo(() => {
    if (!activeWorkspaceId) {
      return EMPTY_SELECTIONS;
    }
    return state.modelSelectionsByWorkspace[activeWorkspaceId] ?? EMPTY_SELECTIONS;
  }, [activeWorkspaceId, state.modelSelectionsByWorkspace]);
  const error = records.error ?? (activeWorkspaceId ? state.errorByWorkspace[activeWorkspaceId] ?? null : null);
  const isSubmitting = activeWorkspaceId
    ? state.submittingByWorkspace[activeWorkspaceId] ?? false
    : false;

  useEffect(() => {
    if (!activeWorkspaceId || !activeWorkspace) {
      return;
    }
    if ((activeWorkspace.kind ?? "main") === "worktree" && runMode !== "local") {
      setState((prev) => ({
        ...prev,
        modeByWorkspace: { ...prev.modeByWorkspace, [activeWorkspaceId]: "local" },
      }));
    }
  }, [activeWorkspace, activeWorkspaceId, runMode]);

  const modelLookup = useMemo(() => {
    const map = new Map<string, ModelOption>();
    models.forEach((model) => {
      map.set(model.id, model);
    });
    return map;
  }, [models]);

  const setDraft = useCallback(
    (value: string) => {
      if (!activeWorkspaceId) {
        return;
      }
      setState((prev) => ({
        ...prev,
        draftsByWorkspace: { ...prev.draftsByWorkspace, [activeWorkspaceId]: value },
        errorByWorkspace: { ...prev.errorByWorkspace, [activeWorkspaceId]: null },
      }));
    },
    [activeWorkspaceId],
  );

  const setRunMode = useCallback(
    (mode: WorkspaceRunMode) => {
      if (!activeWorkspaceId) {
        return;
      }
      setState((prev) => ({
        ...prev,
        modeByWorkspace: { ...prev.modeByWorkspace, [activeWorkspaceId]: mode },
        errorByWorkspace: { ...prev.errorByWorkspace, [activeWorkspaceId]: null },
      }));
    },
    [activeWorkspaceId],
  );

  const toggleModelSelection = useCallback(
    (modelId: string) => {
      if (!activeWorkspaceId) {
        return;
      }
      setState((prev) => {
        const current = prev.modelSelectionsByWorkspace[activeWorkspaceId] ?? {};
        const next = { ...current };
        if (next[modelId]) {
          delete next[modelId];
        } else {
          next[modelId] = 1;
        }
        return {
          ...prev,
          modelSelectionsByWorkspace: {
            ...prev.modelSelectionsByWorkspace,
            [activeWorkspaceId]: next,
          },
          errorByWorkspace: { ...prev.errorByWorkspace, [activeWorkspaceId]: null },
        };
      });
    },
    [activeWorkspaceId],
  );

  const setModelCount = useCallback(
    (modelId: string, count: number) => {
      if (!activeWorkspaceId) {
        return;
      }
      setState((prev) => {
        const current = prev.modelSelectionsByWorkspace[activeWorkspaceId] ?? {};
        const next = { ...current, [modelId]: Math.max(1, count) };
        return {
          ...prev,
          modelSelectionsByWorkspace: {
            ...prev.modelSelectionsByWorkspace,
            [activeWorkspaceId]: next,
          },
          errorByWorkspace: { ...prev.errorByWorkspace, [activeWorkspaceId]: null },
        };
      });
    },
    [activeWorkspaceId],
  );

  const setWorkspaceError = useCallback(
    (message: string | null) => {
      if (!activeWorkspaceId) {
        return;
      }
      setState((prev) => ({
        ...prev,
        errorByWorkspace: { ...prev.errorByWorkspace, [activeWorkspaceId]: message },
      }));
    },
    [activeWorkspaceId],
  );

  const setSubmitting = useCallback(
    (value: boolean) => {
      if (!activeWorkspaceId) {
        return;
      }
      setState((prev) => ({
        ...prev,
        submittingByWorkspace: {
          ...prev.submittingByWorkspace,
          [activeWorkspaceId]: value,
        },
      }));
    },
    [activeWorkspaceId],
  );

  const startRun = useCallback(async (images: string[] = []) => {
    if (!activeWorkspaceId || !activeWorkspace || isSubmitting || launching.current) return false;
    const prompt = draft.trim();
    if (!prompt && !images.length) return false;
    const selectedModels = Object.entries(modelSelections)
      .filter(([modelId, count]) => count > 0 && modelLookup.has(modelId))
      .flatMap(([modelId, count]) => Array.from({length: count}, () => ({modelId, model: modelLookup.get(modelId) ?? null})));
    const codexSelections = runMode === "local"
      ? [{modelId: selectedModelId, model: selectedModelId ? modelLookup.get(selectedModelId) ?? null : null}]
      : selectedModels;
    const selections = runtimeSelection.length
      ? runtimeSelection.flatMap(runtime => runtime.kind === "codex"
        ? (codexSelections.length ? codexSelections : [{modelId: selectedModelId, model: null}]).map(s => ({...s, runtime}))
        : [{modelId: null, model: null, runtime}])
      : codexSelections.map(s => ({...s, runtime: null}));
    if (runMode === "local" && selections.length > 1) { setWorkspaceError("Select one agent for a local run, or use worktrees for multiple agents."); return false; }
    if (!selections.length) { setWorkspaceError("Select at least one model to run in a worktree."); return false; }
    launching.current = true;
    setSubmitting(true);
    setWorkspaceError(null);
    let record: RunRecord;
    const creationKey = JSON.stringify({backendKey, activeWorkspaceId, prompt, images, runMode, selections, effort, serviceTier, collaborationMode});
    if (pendingCreation.current?.key !== creationKey) pendingCreation.current = {key: creationKey, id: crypto.randomUUID()};
    try {
      record = await createRun({requestId: pendingCreation.current.id, workspaceId: activeWorkspaceId, title: buildRunTitle(prompt), prompt, mode: runMode, images,
        instances: selections.map(selection => ({
          ...(selection.runtime && selection.runtime.kind !== "codex" ? {modelLabel: "Agent default"} : {modelId: selection.modelId, model: selection.model?.model ?? null, modelLabel: resolveModelLabel(selection.model, "Default model"), effort, serviceTier, collaborationMode}),
          ...(selection.runtime ? {runtime: {profileId: selection.runtime.id, profileRevision: selection.runtime.revision, kind: selection.runtime.kind}, runtimeProfileId: selection.runtime.id, runtimeName: selection.runtime.name} : {}),
        })),
      });
    } catch (error) {
      setWorkspaceError(`Run recording failed; no instances launched: ${String(error)}`);
      launching.current = false;
      setSubmitting(false);
      return false;
    }
    pendingCreation.current = null;
    setState(previous => ({...previous, draftsByWorkspace: {...previous.draftsByWorkspace, [activeWorkspaceId]: ""}}));
    await records.refresh();
    let branchBase = buildWorktreeBranch(prompt);
    const metadataTask = async () => {
      try {
        const metadata = await generateRunMetadata(activeWorkspace.id, prompt);
        branchBase = normalizeWorktreeName(metadata?.worktreeName) ?? branchBase;
        if (metadata?.title?.trim()) await updateRunMetadata(record.id, record.revision, metadata.title.trim());
        await records.refresh();
      } catch { /* Metadata is optional; durable launch identity already exists. */ }
    };
    if (selections.every(s => !s.runtime)) {
      if (runMode === "worktree") await metadataTask();
      else void metadataTask();
    }
    try {
      for (const [ordinal, selection] of selections.entries()) {
        const instance = record.instances[ordinal];
        const runContext = {runId: record.id, instanceId: instance.id};
        try {
          let workspace = activeWorkspace;
          if (runMode === "worktree") {
            const created = await addWorktreeAgent(activeWorkspace, `${branchBase}-${record.id.slice(0, 8)}-${ordinal + 1}`, {activate: false, runContext});
            if (!created) throw new Error("Failed to create worktree; inspect its durable receipt before retrying.");
            workspace = created;
          }
          if (!selection.runtime && !workspace.connected) await connectWorkspace(workspace);
          if (runMode === "worktree") {
            let observation: SetupObservation = {status: "not_configured"};
            try { observation = await onWorktreeCreated?.(workspace, activeWorkspace) ?? observation; }
            catch (error) { observation = {status: "dispatch_failed", message: String(error)}; }
            await recordRunSetup(instance.id, observation);
          }
          if (selection.runtime) {
            const session = await runtimeRequest<RuntimeSession>("create_session", {workspaceId: workspace.id, profileId: selection.runtime.id, expectedProfileRevision: selection.runtime.revision});
            setOpenedRuntimeSessionId(session.sessionId);
            const content = await buildRuntimePrompt(prompt, images, selection.runtime.kind);
            const options = selection.runtime.kind === "codex" ? {model: selection.model?.model ?? null, effort, ...(serviceTier !== undefined ? {serviceTier} : {}), ...(collaborationMode ? {collaborationMode} : {})} : {};
            await runtimeRequest("submit", {requestId: crypto.randomUUID(), sessionId: session.sessionId, prompt: content, options, runContext});
            continue;
          }
          const threadId = await startThreadForWorkspace(workspace.id, {activate: false, runContext});
          if (!threadId) throw new Error("Thread creation did not return a receipt; reconcile this instance.");
          seedThreadCodexParams?.(workspace.id, threadId, {modelId: selection.modelId, effort, serviceTier});
          const result = await sendUserMessageToThread(workspace, threadId, prompt, images, {
            model: selection.model?.model ?? null, effort, serviceTier, collaborationMode, runContext,
          });
          // A lost IPC reply can hide a committed receipt. Read it; never resend.
          const latest = await getRun(record.id);
          const submitted = latest.instances.find(i => i.id === instance.id);
          if (submitted?.submissionOutcome !== "accepted") {
            throw new Error(result?.runReceipt?.message ?? `Submission ${submitted?.submissionOutcome ?? "unknown"}; no automatic retry`);
          }
        } catch (error) {
          const message = error instanceof Error ? error.message : String(error);
          try { await recordRunFailure(instance.id, message); }
          catch (recordError) { setWorkspaceError(`Could not save run failure: ${String(recordError)}. ${message}`); }
        }
        await records.refresh();
      }
    } finally {
      launching.current = false;
      setSubmitting(false);
      await records.refresh();
    }
    return true;
  }, [activeWorkspace, activeWorkspaceId, backendKey, isSubmitting, draft, modelSelections, modelLookup, runMode, selectedModelId,
    effort, serviceTier, collaborationMode, runtimeSelection, setSubmitting, setWorkspaceError, records.refresh, addWorktreeAgent,
    connectWorkspace, onWorktreeCreated, startThreadForWorkspace, seedThreadCodexParams, sendUserMessageToThread]);

  return {
    runtimeSelection, setRuntimeSelection, openedRuntimeSessionId,
    runs,
    draft,
    runMode,
    modelSelections,
    error,
    isSubmitting,
    setDraft,
    setRunMode,
    toggleModelSelection,
    setModelCount,
    startRun,
  };
}

export function toWorkspaceHomeRun(record: RunRecord): WorkspaceHomeRun {
  const errors = record.instances.flatMap(i => i.error ? [{message: i.error}] : []);
  const accepted = record.instances.filter(i => i.submissionOutcome === "accepted").length;
  const uncertain = record.instances.some(i => i.submissionOutcome === "unknown" || i.preparation === "unknown");
  const pending = record.instances.some(i => i.preparation !== "failed" && ["not_submitted", "submitting"].includes(i.submissionOutcome));
  return {record, id: record.id, workspaceId: record.workspaceId, title: record.title, prompt: record.prompt, createdAt: record.createdAt, mode: record.mode,
    status: pending ? "pending" : accepted === record.instances.length ? "ready" : accepted ? "partial" : uncertain ? "unknown" : "failed", error: null, instanceErrors: errors,
    instances: record.instances.map(i => ({id: i.id, workspaceId: i.workspaceId ?? "", threadId: i.threadId ?? "", sessionId: i.sessionId, modelId: i.requested.modelId ?? null, modelLabel: i.requested.runtimeName ? `${i.requested.runtimeName} · ${i.requested.modelLabel ?? "Agent default"}` : i.requested.modelLabel ?? "Default model", sequence: i.ordinal + 1,
      durableStatus: `${i.preparation} · ${i.submissionOutcome} · ${i.executionOutcome}`})),
  };
}
