import { exportLegacyUiStateSnapshot } from "@/services/tauri";

const PANEL_VALUE_KEYS = [
  "sidebarWidth",
  "rightPanelWidth",
  "chatDiffSplitPositionPercent",
  "planPanelHeight",
  "terminalPanelHeight",
  "debugPanelHeight",
] as const;
const PANEL_BOOLEAN_KEYS = ["sidebarCollapsed", "rightPanelCollapsed"] as const;
const PROMPT_HISTORY_PREFIX = "codexmonitor.promptHistory.";

export type LegacyUiStateV1 = {
  schemaVersion: 1;
  revision: number;
  panels: Record<string, number | boolean>;
  collapsedGroups: Record<string, boolean>;
  threadListSortKey: "created_at" | "updated_at" | null;
  threadListOrganizeMode:
    | "by_project"
    | "by_project_activity"
    | "threads_only"
    | null;
  pinnedThreads: Record<string, number>;
  customNames: Record<string, string>;
  threadCodexParams: Record<string, Record<string, unknown>>;
  promptHistory: Record<string, string[]>;
};

export type LegacyUiStateSnapshot = {
  schemaVersion: 1;
  exportedAtMs: number;
  state: LegacyUiStateV1;
};

function parseObject(raw: string | null): Record<string, unknown> {
  if (!raw) return {};
  try {
    const value: unknown = JSON.parse(raw);
    return value !== null && typeof value === "object" && !Array.isArray(value)
      ? (value as Record<string, unknown>)
      : {};
  } catch {
    return {};
  }
}

function parseRecord(key: string): Record<string, unknown> {
  try {
    return parseObject(window.localStorage.getItem(key));
  } catch {
    return {};
  }
}

function exportPanels(): LegacyUiStateV1["panels"] {
  const result: LegacyUiStateV1["panels"] = {};
  for (const key of PANEL_VALUE_KEYS) {
    try {
      const raw = window.localStorage.getItem(`codexmonitor.${key}`);
      const value = raw === null ? NaN : Number(raw);
      if (Number.isFinite(value)) result[key] = value;
    } catch {
      // One unavailable key does not prevent exporting the others.
    }
  }
  for (const key of PANEL_BOOLEAN_KEYS) {
    try {
      const raw = window.localStorage.getItem(`codexmonitor.${key}`);
      if (raw === "true" || raw === "false") result[key] = raw === "true";
    } catch {
      // One unavailable key does not prevent exporting the others.
    }
  }
  return result;
}

function exportPromptHistory(): Record<string, string[]> {
  const result: Record<string, string[]> = {};
  try {
    for (let index = 0; index < window.localStorage.length; index += 1) {
      const key = window.localStorage.key(index);
      if (!key?.startsWith(PROMPT_HISTORY_PREFIX)) continue;
      const historyKey = key.slice(PROMPT_HISTORY_PREFIX.length);
      if (!historyKey) continue;
      const value = parseObjectValue(window.localStorage.getItem(key));
      if (Array.isArray(value)) {
        result[historyKey] = value.filter((item): item is string => typeof item === "string").slice(-200);
      }
    }
  } catch {
    // Storage access can be denied by the host; return the partial snapshot.
  }
  return result;
}

function exportCollapsedGroups(): Record<string, boolean> {
  const result: Record<string, boolean> = {};
  try {
    const parsed = parseObjectValue(window.localStorage.getItem("codexmonitor.collapsedGroups"));
    if (Array.isArray(parsed)) {
      for (const id of parsed) if (typeof id === "string") result[id] = true;
    }
  } catch {
    // Preserve the other presentation settings if storage is unavailable.
  }
  return result;
}

function parseObjectValue(raw: string | null): unknown {
  if (!raw) return null;
  try {
    return JSON.parse(raw);
  } catch {
    return null;
  }
}

/** Read only presentation keys needed by the native desktop handoff. */
export function collectLegacyUiStateSnapshot(nowMs = Date.now()): LegacyUiStateSnapshot {
  const pinnedThreadsRaw = parseRecord("codexmonitor.pinnedThreads");
  const customNamesRaw = parseRecord("codexmonitor.threadCustomNames");
  const paramsRaw = parseRecord("codexmonitor.threadCodexParams");

  const collapsedGroups = exportCollapsedGroups();
  const pinnedThreads: Record<string, number> = {};
  for (const [key, value] of Object.entries(pinnedThreadsRaw)) {
    if (typeof value === "number" && Number.isFinite(value)) pinnedThreads[key] = value;
  }
  const customNames: Record<string, string> = {};
  for (const [key, value] of Object.entries(customNamesRaw)) {
    if (typeof value === "string") customNames[key] = value;
  }
  const threadCodexParams: Record<string, Record<string, unknown>> = {};
  const paramKeys = new Set([
    "modelId",
    "effort",
    "serviceTier",
    "accessMode",
    "collaborationModeId",
    "codexArgsOverride",
    "updatedAt",
  ]);
  for (const [key, value] of Object.entries(paramsRaw)) {
    if (value !== null && typeof value === "object" && !Array.isArray(value)) {
      const safe: Record<string, unknown> = {};
      for (const [field, fieldValue] of Object.entries(value)) {
        if (paramKeys.has(field) && (fieldValue === null || ["string", "number"].includes(typeof fieldValue))) {
          safe[field] = fieldValue;
        }
      }
      threadCodexParams[key] = safe;
    }
  }

  let threadListSortKey: LegacyUiStateV1["threadListSortKey"] = null;
  let threadListOrganizeMode: LegacyUiStateV1["threadListOrganizeMode"] = null;
  try {
    const sort = window.localStorage.getItem("codexmonitor.threadListSortKey");
    if (sort === "created_at" || sort === "updated_at") threadListSortKey = sort;
    const mode = window.localStorage.getItem("codexmonitor.threadListOrganizeMode");
    if (mode === "by_project" || mode === "by_project_activity" || mode === "threads_only") {
      threadListOrganizeMode = mode;
    }
  } catch {
    // Preserve an otherwise useful partial export.
  }

  const state: LegacyUiStateV1 = {
    schemaVersion: 1,
    revision: 0,
    panels: exportPanels(),
    collapsedGroups,
    threadListSortKey,
    threadListOrganizeMode,
    pinnedThreads,
    customNames,
    threadCodexParams,
    promptHistory: exportPromptHistory(),
  };
  return { schemaVersion: 1, exportedAtMs: nowMs, state };
}

/** Persist an allowlisted snapshot through the local Rust command. */
export async function exportLegacyUiState(): Promise<string> {
  const snapshot = collectLegacyUiStateSnapshot();
  const result = await exportLegacyUiStateSnapshot(snapshot);
  return result.path;
}
