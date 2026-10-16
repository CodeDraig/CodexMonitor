import { normalizeListIndentation } from "./normalizeListIndentation";
import { normalizeStructuredReviewTables } from "./structuredReviewTables";

function stripTrailingMemoryCitation(value: string) {
  return value.replace(/\n*<oai-mem-citation>[\s\S]*?<\/oai-mem-citation>\s*$/i, "").trim();
}

export function isStandaloneMarkdownTable(value: string) {
  const stripped = stripTrailingMemoryCitation(value);
  if (!stripped) {
    return false;
  }
  const normalized = normalizeStructuredReviewTables(normalizeListIndentation(stripped)).trim();
  if (!normalized) {
    return false;
  }
  const lines = normalized.split(/\r?\n/).filter((line) => line.trim().length > 0);
  if (lines.length < 2) {
    return false;
  }
  return lines.every((line) => /^\|.*\|\s*$/.test(line.trim()));
}
