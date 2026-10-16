import { memo } from "react";
import type { ConversationItem } from "@/types";
import type { MarkdownFileLinkProps } from "./messageRowTypes";
import { Markdown } from "./Markdown";

type ReviewRowProps = MarkdownFileLinkProps & {
  item: Extract<ConversationItem, { kind: "review" }>;
};

export const ReviewRow = memo(function ReviewRow({
  item,
  showMessageFilePath,
  workspacePath,
  onOpenFileLink,
  onOpenFileLinkMenu,
  onOpenThreadLink,
}: ReviewRowProps) {
  const title = item.state === "started" ? "Review started" : "Review completed";
  return (
    <div className="item-card review">
      <div className="review-header">
        <span className="review-title">{title}</span>
        <span
          className={`review-badge ${item.state === "started" ? "active" : "done"}`}
        >
          Review
        </span>
      </div>
      {item.text && (
        <Markdown
          value={item.text}
          className="item-text markdown"
          showFilePath={showMessageFilePath}
          workspacePath={workspacePath}
          onOpenFileLink={onOpenFileLink}
          onOpenFileLinkMenu={onOpenFileLinkMenu}
          onOpenThreadLink={onOpenThreadLink}
        />
      )}
    </div>
  );
});
