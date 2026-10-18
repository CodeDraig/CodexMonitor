import {AcpContent} from "@/features/runtime/components/AcpContent";
import { memo, useMemo, useState } from "react";
import Check from "lucide-react/dist/esm/icons/check";
import Copy from "lucide-react/dist/esm/icons/copy";
import Quote from "lucide-react/dist/esm/icons/quote";
import type { ConversationItem } from "@/types";
import type { MarkdownFileLinkProps } from "./messageRowTypes";
import { Markdown } from "./Markdown";
import { isStandaloneMarkdownTable } from "@/features/messages/utils/standaloneMarkdownTable";
import { useMessageQuote } from "../hooks/useMessageQuote";
import { MessageImageGrid, ImageLightbox } from "./MessageImages";
import { normalizeMessageImageSrc, type MessageImage } from "../utils/messageRenderUtils";

type MessageRowProps = MarkdownFileLinkProps & {
  item: Extract<ConversationItem, { kind: "message" }>;
  isCopied: boolean;
  onCopy: (item: Extract<ConversationItem, { kind: "message" }>) => void;
  onQuote?: (item: Extract<ConversationItem, { kind: "message" }>, selectedText?: string) => void;
  codeBlockCopyUseModifier?: boolean;
};

export const MessageRow = memo(function MessageRow({
  item,
  isCopied,
  onCopy,
  onQuote,
  codeBlockCopyUseModifier,
  showMessageFilePath,
  workspacePath,
  onOpenFileLink,
  onOpenFileLinkMenu,
  onOpenThreadLink,
}: MessageRowProps) {
  const [lightboxIndex, setLightboxIndex] = useState<number | null>(null);
  const { bubbleRef, captureSelection, handleQuote } = useMessageQuote(item, onQuote);
  const hasText = item.text.trim().length > 0;
  const imageItems = useMemo(() => {
    if (!item.images || item.images.length === 0) {
      return [];
    }
    return item.images
      .map((image, index) => {
        const src = normalizeMessageImageSrc(image);
        if (!src) {
          return null;
        }
        return { src, label: `Image ${index + 1}` };
      })
      .filter(Boolean) as MessageImage[];
  }, [item.images]);
  const isTableOnlyAssistantMessage =
    item.role === "assistant" &&
    hasText &&
    imageItems.length === 0 &&
    isStandaloneMarkdownTable(item.text);


  return (
    <div className={`message ${item.role}`}>
      <div
        ref={bubbleRef}
        className={`bubble message-bubble${isTableOnlyAssistantMessage ? " message-bubble-table-only" : ""}`}
      >
        {imageItems.length > 0 && (
          <MessageImageGrid
            images={imageItems}
            onOpen={setLightboxIndex}
            hasText={hasText}
          />
        )}
        {hasText && (
          <Markdown
            value={item.text}
            className="markdown"
            codeBlockStyle="message"
            codeBlockCopyUseModifier={codeBlockCopyUseModifier}
            showFilePath={showMessageFilePath}
            workspacePath={workspacePath}
            onOpenFileLink={onOpenFileLink}
            onOpenFileLinkMenu={onOpenFileLinkMenu}
            onOpenThreadLink={onOpenThreadLink}
          />
        )}
        {item.acpContent && <AcpContent {...item.acpContent}/>}
        {lightboxIndex !== null && imageItems.length > 0 && (
          <ImageLightbox
            images={imageItems}
            activeIndex={lightboxIndex}
            onClose={() => setLightboxIndex(null)}
          />
        )}
        {onQuote && hasText && (
          <button
            type="button"
            className="ghost message-quote-button"
            onMouseDown={captureSelection}
            onTouchStart={captureSelection}
            onClick={handleQuote}
            aria-label="Quote message"
            title="Quote message"
          >
            <Quote size={14} aria-hidden />
          </button>
        )}
        <button
          type="button"
          className={`ghost message-copy-button${isCopied ? " is-copied" : ""}`}
          onClick={() => onCopy(item)}
          aria-label="Copy message"
          title="Copy message"
        >
          <span className="message-copy-icon" aria-hidden>
            <Copy className="message-copy-icon-copy" size={14} />
            <Check className="message-copy-icon-check" size={14} />
          </span>
        </button>
      </div>
    </div>
  );
});
