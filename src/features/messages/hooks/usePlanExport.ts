import { useCallback, useState, type MouseEvent } from "react";
import { exportMarkdownFile } from "@services/tauri";
import { pushErrorToast } from "@services/toasts";

function buildPlanExportFileName(itemId: string) {
  const normalized = itemId
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9_-]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 48);
  if (!normalized) {
    return "plan.md";
  }
  return normalized.startsWith("plan-") ? `${normalized}.md` : `plan-${normalized}.md`;
}

export function usePlanExport(itemId: string, output: string | undefined) {
  const [isExportingPlan, setIsExportingPlan] = useState(false);
  const handlePlanExport = useCallback(
    async (event: MouseEvent<HTMLButtonElement>) => {
      event.preventDefault();
      event.stopPropagation();
      const markdown = (output ?? "").trim();
      if (!markdown) {
        return;
      }
      setIsExportingPlan(true);
      try {
        await exportMarkdownFile(markdown, buildPlanExportFileName(itemId));
      } catch (error) {
        const message = error instanceof Error ? error.message : "Unable to export plan.";
        pushErrorToast({
          title: "Plan export failed",
          message,
        });
      } finally {
        setIsExportingPlan(false);
      }
    },
    [itemId, output],
  );

  return { isExportingPlan, handlePlanExport };
}
