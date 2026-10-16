import type { CollaborationModeOption } from "@/types";

export function parseCollaborationModes(response: { result?: { data?: unknown } }): CollaborationModeOption[] {
  const rawData: unknown[] = Array.isArray(response?.result?.data) ? response.result.data : [];
  return rawData
  .map((item: any) => {
    if (!item || typeof item !== "object") {
      return null;
    }
    const modeId = String(item.mode ?? item.name ?? "").trim();
    if (!modeId) {
      return null;
    }

    const model = String(item.model ?? "");
    const reasoningEffort = item.reasoning_effort ?? null;
    const labelSource = typeof item.name === "string" && item.name.trim() ? item.name : modeId;

    const option: CollaborationModeOption = {
      id: modeId,
      label: labelSource,
      mode: modeId,
      model,
      reasoningEffort: reasoningEffort ? String(reasoningEffort) : null,
      developerInstructions: null,
      value: item as Record<string, unknown>,
    };
    return option;
  })
  .filter((mode): mode is CollaborationModeOption => mode !== null);
}
