import { memo, useEffect, useState } from "react";
import { usePlanExport } from "../hooks/usePlanExport";
import type { ConversationItem } from "@/types";
import type { MarkdownFileLinkProps } from "./messageRowTypes";
import { Markdown } from "./Markdown";
import { PierreDiffBlock } from "@/features/git/components/PierreDiffBlock";
import { basename, buildToolSummary, formatToolStatusLabel, toolStatusTone } from "../utils/messageRenderUtils";
import { CommandOutput } from "./CommandOutput";
import { toolIconForSummary } from "./toolIcons";

type ToolRowProps = MarkdownFileLinkProps & {
  item: Extract<ConversationItem, { kind: "tool" }>;
  isExpanded: boolean;
  onToggle: (id: string) => void;
  onRequestAutoScroll?: () => void;
};

export const ToolRow = memo(function ToolRow({
  item,
  isExpanded,
  onToggle,
  showMessageFilePath,
  workspacePath,
  onOpenFileLink,
  onOpenFileLinkMenu,
  onOpenThreadLink,
  onRequestAutoScroll,
}: ToolRowProps) {
  const isFileChange = item.toolType === "fileChange";
  const isCommand = item.toolType === "commandExecution";
  const isPlan = item.toolType === "plan";
  const commandText = isCommand
    ? item.title.replace(/^Command:\s*/i, "").trim()
    : "";
  const summary = buildToolSummary(item, commandText);
  const changeNames = (item.changes ?? [])
    .map((change) => basename(change.path))
    .filter(Boolean);
  const hasChanges = changeNames.length > 0;
  const tone = toolStatusTone(item, hasChanges);
  const ToolIcon = toolIconForSummary(item, summary);
  const summaryLabel = isFileChange
    ? changeNames.length > 1
      ? "files edited"
      : "file edited"
    : isCommand
      ? ""
      : summary.label;
  const inlineStatus = formatToolStatusLabel(item);
  const summaryValue = isFileChange
    ? changeNames.length > 1
      ? `${changeNames[0]} +${changeNames.length - 1}`
      : changeNames[0] || "changes"
    : summary.value;
  const shouldFadeCommand =
    isCommand && !isExpanded && (summaryValue?.length ?? 0) > 80;
  const showToolOutput = isExpanded && (!isFileChange || !hasChanges);
  const normalizedStatus = (item.status ?? "").toLowerCase();
  const isCommandRunning = isCommand && /in[_\s-]*progress|running|started/.test(normalizedStatus);
  const commandDurationMs =
    typeof item.durationMs === "number" ? item.durationMs : null;
  const isLongRunning = commandDurationMs !== null && commandDurationMs >= 1200;
  const [showLiveOutput, setShowLiveOutput] = useState(false);
  const { isExportingPlan, handlePlanExport } = usePlanExport(item.id, summary.output);

  useEffect(() => {
    if (!isCommandRunning) {
      setShowLiveOutput(false);
      return;
    }
    const timeoutId = window.setTimeout(() => {
      setShowLiveOutput(true);
    }, 600);
    return () => {
      window.clearTimeout(timeoutId);
    };
  }, [isCommandRunning]);

  const showCommandOutput =
    isCommand &&
    summary.output &&
    (isExpanded || (isCommandRunning && showLiveOutput) || isLongRunning);

  useEffect(() => {
    if (showCommandOutput && isCommandRunning && showLiveOutput) {
      onRequestAutoScroll?.();
    }
  }, [isCommandRunning, onRequestAutoScroll, showCommandOutput, showLiveOutput]);


  return (
    <div className={`tool-inline tool-inline-row ${isExpanded ? "tool-inline-expanded" : ""}`}>
      <button
        type="button"
        className="tool-inline-bar-toggle"
        onClick={() => onToggle(item.id)}
        aria-expanded={isExpanded}
        aria-label="Toggle tool details"
      />
      <div className="tool-inline-content">
        <button
          type="button"
          className="tool-inline-summary tool-inline-toggle"
          onClick={() => onToggle(item.id)}
          aria-expanded={isExpanded}
        >
          <ToolIcon className={`tool-inline-icon ${tone}`} size={14} aria-hidden />
          {summaryLabel && (
            <span className="tool-inline-label">{summaryLabel}:</span>
          )}
          {summaryValue && (
            <span
              className={`tool-inline-value ${isCommand ? "tool-inline-command" : ""} ${
                isCommand && isExpanded ? "tool-inline-command-full" : ""
              }`}
            >
              {isCommand ? (
                <span
                  className={`tool-inline-command-text ${
                    shouldFadeCommand ? "tool-inline-command-fade" : ""
                  }`}
                >
                  {summaryValue}
                </span>
              ) : (
                summaryValue
              )}
            </span>
          )}
          {inlineStatus && (
            <span className="tool-inline-status">{inlineStatus}</span>
          )}
        </button>
        {isExpanded && summary.detail && !isFileChange && (
          <div className="tool-inline-detail">{summary.detail}</div>
        )}
        {isExpanded && isCommand && item.detail && (
          <div className="tool-inline-detail tool-inline-muted">
            cwd: {item.detail}
          </div>
        )}
        {isExpanded && isFileChange && hasChanges && (
          <div className="tool-inline-change-list">
            {item.changes?.map((change, index) => (
              <div
                key={`${change.path}-${index}`}
                className="tool-inline-change"
              >
                <div className="tool-inline-change-header">
                  {change.kind && (
                    <span className="tool-inline-change-kind">
                      {change.kind.toUpperCase()}
                    </span>
                  )}
                  <span className="tool-inline-change-path">
                    {basename(change.path)}
                  </span>
                </div>
                {change.diff && (
                  <div className="diff-viewer-output">
                    <PierreDiffBlock diff={change.diff} displayPath={change.path} />
                  </div>
                )}
              </div>
            ))}
          </div>
        )}
        {isExpanded && isFileChange && !hasChanges && item.detail && (
          <Markdown
            value={item.detail}
            className="item-text markdown"
            showFilePath={showMessageFilePath}
            workspacePath={workspacePath}
            onOpenFileLink={onOpenFileLink}
            onOpenFileLinkMenu={onOpenFileLinkMenu}
            onOpenThreadLink={onOpenThreadLink}
          />
        )}
        {showCommandOutput && <CommandOutput output={summary.output ?? ""} />}
        {showToolOutput && summary.output && !isCommand && (
          <Markdown
            value={summary.output}
            className="tool-inline-output markdown"
            codeBlock={item.toolType !== "plan"}
            showFilePath={showMessageFilePath}
            workspacePath={workspacePath}
            onOpenFileLink={onOpenFileLink}
            onOpenFileLinkMenu={onOpenFileLinkMenu}
            onOpenThreadLink={onOpenThreadLink}
          />
        )}
        {showToolOutput && isPlan && (summary.output ?? "").trim() && (
          <div className="tool-inline-actions">
            <button
              type="button"
              className="ghost tool-inline-action"
              onClick={handlePlanExport}
              disabled={isExportingPlan}
            >
              {isExportingPlan ? "Exporting..." : "Export .md"}
            </button>
          </div>
        )}
      </div>
    </div>
  );
});
