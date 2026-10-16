import { useCallback, useRef } from "react";
import type { ConversationItem } from "@/types";

type Message = Extract<ConversationItem, { kind: "message" }>;

export function useMessageQuote(item: Message, onQuote?: (item: Message, selectedText?: string) => void) {
  const bubbleRef = useRef<HTMLDivElement | null>(null);
  const selectionSnapshotRef = useRef<string | null>(null);
  const getSelectedMessageText = useCallback(() => {
    const bubble = bubbleRef.current;
    const selection = window.getSelection();
    if (!bubble || !selection || selection.rangeCount === 0 || selection.isCollapsed) {
      return null;
    }
    const selectedText = selection.toString().trim();
    if (!selectedText) {
      return null;
    }
    const range = selection.getRangeAt(0);
    if (!bubble.contains(range.commonAncestorContainer)) {
      return null;
    }

    const isWithinMessageControls = (node: Node | null) => {
      if (!node) {
        return false;
      }
      const element = node instanceof Element ? node : node.parentElement;
      return Boolean(element?.closest(".message-quote-button, .message-copy-button"));
    };

    if (isWithinMessageControls(selection.anchorNode) || isWithinMessageControls(selection.focusNode)) {
      return null;
    }
    return selectedText;
  }, []);

  const handleQuote = useCallback(() => {
    if (!onQuote) {
      return;
    }
    const selectedText = getSelectedMessageText() ?? selectionSnapshotRef.current ?? undefined;
    selectionSnapshotRef.current = null;
    onQuote(item, selectedText);
  }, [getSelectedMessageText, item, onQuote]);

  const captureSelection = () => {
    selectionSnapshotRef.current = getSelectedMessageText();
  };
  return { bubbleRef, captureSelection, handleQuote };
}
