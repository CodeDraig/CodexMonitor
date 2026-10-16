import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { MAX_COMMAND_OUTPUT_LINES } from "../utils/messageRenderUtils";

type CommandOutputProps = {
  output: string;
};

export const CommandOutput = memo(function CommandOutput({ output }: CommandOutputProps) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const [isPinned, setIsPinned] = useState(true);
  const lines = useMemo(() => {
    if (!output) {
      return [];
    }
    return output.split(/\r?\n/);
  }, [output]);
  const lineWindow = useMemo(() => {
    if (lines.length <= MAX_COMMAND_OUTPUT_LINES) {
      return { offset: 0, lines };
    }
    const startIndex = lines.length - MAX_COMMAND_OUTPUT_LINES;
    return { offset: startIndex, lines: lines.slice(startIndex) };
  }, [lines]);

  const handleScroll = useCallback(() => {
    const node = containerRef.current;
    if (!node) {
      return;
    }
    const threshold = 6;
    const distanceFromBottom = node.scrollHeight - node.scrollTop - node.clientHeight;
    setIsPinned(distanceFromBottom <= threshold);
  }, []);

  useEffect(() => {
    const node = containerRef.current;
    if (!node || !isPinned) {
      return;
    }
    node.scrollTop = node.scrollHeight;
  }, [lineWindow, isPinned]);

  if (lineWindow.lines.length === 0) {
    return null;
  }

  return (
    <div className="tool-inline-terminal" role="log" aria-live="polite">
      <div
        className="tool-inline-terminal-lines"
        ref={containerRef}
        onScroll={handleScroll}
      >
        {lineWindow.lines.map((line, index) => (
          <div
            key={`${lineWindow.offset + index}-${line}`}
            className="tool-inline-terminal-line"
          >
            {line || " "}
          </div>
        ))}
      </div>
    </div>
  );
});
