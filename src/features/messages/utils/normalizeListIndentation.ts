export function normalizeListIndentation(value: string) {
  const lines = value.split(/\r?\n/);
  let inFence = false;
  let activeOrderedItem = false;
  let orderedBaseIndent = 4;
  let orderedIndentOffset: number | null = null;

  const countLeadingSpaces = (line: string) =>
    line.match(/^\s*/)?.[0].length ?? 0;
  const spaces = (count: number) => " ".repeat(Math.max(0, count));
  const normalized = lines.map((line) => {
    const fenceMatch = line.match(/^\s*(```|~~~)/);
    if (fenceMatch) {
      inFence = !inFence;
      activeOrderedItem = false;
      orderedIndentOffset = null;
      return line;
    }
    if (inFence) {
      return line;
    }
    if (!line.trim()) {
      return line;
    }

    const orderedMatch = line.match(/^(\s*)\d+\.\s+/);
    if (orderedMatch) {
      const rawIndent = orderedMatch[1].length;
      const normalizedIndent =
        rawIndent > 0 && rawIndent < 4 ? 4 : rawIndent;
      activeOrderedItem = true;
      orderedBaseIndent = normalizedIndent + 4;
      orderedIndentOffset = null;
      if (normalizedIndent !== rawIndent) {
        return `${spaces(normalizedIndent)}${line.trimStart()}`;
      }
      return line;
    }

    const bulletMatch = line.match(/^(\s*)([-*+])\s+/);
    if (bulletMatch) {
      const rawIndent = bulletMatch[1].length;
      let targetIndent = rawIndent;

      if (!activeOrderedItem && rawIndent > 0 && rawIndent < 4) {
        targetIndent = 4;
      }

      if (activeOrderedItem) {
        if (orderedIndentOffset === null && rawIndent < orderedBaseIndent) {
          orderedIndentOffset = orderedBaseIndent - rawIndent;
        }
        if (orderedIndentOffset !== null) {
          const adjustedIndent = rawIndent + orderedIndentOffset;
          if (adjustedIndent <= orderedBaseIndent + 12) {
            targetIndent = adjustedIndent;
          }
        }
      }

      if (targetIndent !== rawIndent) {
        return `${spaces(targetIndent)}${line.trimStart()}`;
      }
      return line;
    }

    const leadingSpaces = countLeadingSpaces(line);
    if (activeOrderedItem && leadingSpaces < orderedBaseIndent) {
      activeOrderedItem = false;
      orderedIndentOffset = null;
    }
    return line;
  });
  return normalized.join("\n");
}
