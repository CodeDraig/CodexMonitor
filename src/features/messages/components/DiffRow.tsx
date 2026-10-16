import { memo } from "react";
import type { ConversationItem } from "@/types";
import { PierreDiffBlock } from "@/features/git/components/PierreDiffBlock";

type DiffRowProps = {
  item: Extract<ConversationItem, { kind: "diff" }>;
};

export const DiffRow = memo(function DiffRow({ item }: DiffRowProps) {
  return (
    <div className="item-card diff">
      <div className="diff-header">
        <span className="diff-title">{item.title}</span>
        {item.status && <span className="item-status">{item.status}</span>}
      </div>
      <div className="diff-viewer-output">
        <PierreDiffBlock diff={item.diff} displayPath={item.title} />
      </div>
    </div>
  );
});
