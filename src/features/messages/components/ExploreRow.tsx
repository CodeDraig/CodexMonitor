import { memo } from "react";
import Terminal from "lucide-react/dist/esm/icons/terminal";
import type { ConversationItem } from "@/types";
import { exploreKindLabel } from "../utils/messageRenderUtils";

type ExploreRowProps = {
  item: Extract<ConversationItem, { kind: "explore" }>;
};

export const ExploreRow = memo(function ExploreRow({ item }: ExploreRowProps) {
  const title = item.status === "exploring" ? "Exploring" : "Explored";
  return (
    <div className="tool-inline explore-inline">
      <div className="tool-inline-bar-toggle" aria-hidden />
      <div className="tool-inline-content">
        <div className="explore-inline-header">
          <Terminal
            className={`tool-inline-icon ${
              item.status === "exploring" ? "processing" : "completed"
            }`}
            size={14}
            aria-hidden
          />
          <span className="explore-inline-title">{title}</span>
        </div>
        <div className="explore-inline-list">
          {item.entries.map((entry, index) => (
            <div key={`${entry.kind}-${entry.label}-${index}`} className="explore-inline-item">
              <span className="explore-inline-kind">{exploreKindLabel(entry.kind)}</span>
              <span className="explore-inline-label">{entry.label}</span>
              {entry.detail && entry.detail !== entry.label && (
                <span className="explore-inline-detail">{entry.detail}</span>
              )}
            </div>
          ))}
        </div>
      </div>
    </div>
  );
});
