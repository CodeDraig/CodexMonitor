import {AcpContent} from "@/features/runtime/components/AcpContent";
import { memo } from "react";
import Brain from "lucide-react/dist/esm/icons/brain";
import type { ConversationItem } from "@/types";
import type { MarkdownFileLinkProps } from "./messageRowTypes";
import { Markdown } from "./Markdown";
import type { ParsedReasoning, StatusTone } from "../utils/messageRenderUtils";

type ReasoningRowProps = MarkdownFileLinkProps & {
  item: Extract<ConversationItem, { kind: "reasoning" }>;
  parsed: ParsedReasoning;
  isExpanded: boolean;
  onToggle: (id: string) => void;
};

export const ReasoningRow = memo(function ReasoningRow({
  item,
  parsed,
  isExpanded,
  onToggle,
  showMessageFilePath,
  workspacePath,
  onOpenFileLink,
  onOpenFileLinkMenu,
  onOpenThreadLink,
}: ReasoningRowProps) {
  const { summaryTitle, bodyText, hasBody } = parsed;
  const reasoningTone: StatusTone = hasBody ? "completed" : "processing";
  return (
    <div className="tool-inline reasoning-inline">
      <button
        type="button"
        className="tool-inline-bar-toggle"
        onClick={() => onToggle(item.id)}
        aria-expanded={isExpanded}
        aria-label="Toggle reasoning details"
      />
      <div className="tool-inline-content">
        <button
          type="button"
          className="tool-inline-summary tool-inline-toggle"
          onClick={() => onToggle(item.id)}
          aria-expanded={isExpanded}
        >
          <Brain
            className={`tool-inline-icon ${reasoningTone}`}
            size={14}
            aria-hidden
          />
          <span className="tool-inline-value">{summaryTitle}</span>
        </button>
        {hasBody && (
          <Markdown
            value={bodyText}
            className={`reasoning-inline-detail markdown ${
              isExpanded ? "" : "tool-inline-clamp"
            }`}
            showFilePath={showMessageFilePath}
            workspacePath={workspacePath}
            onOpenFileLink={onOpenFileLink}
            onOpenFileLinkMenu={onOpenFileLinkMenu}
            onOpenThreadLink={onOpenThreadLink}
          />
        )}
        {isExpanded&&item.acpContent&&<AcpContent {...item.acpContent}/>}
      </div>
    </div>
  );
});
