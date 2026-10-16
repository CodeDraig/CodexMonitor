type StructuredReviewFinding = {
  file: string;
  category: string;
  finding: string;
  recommendation: string;
  severity: string;
};

function escapeTableCell(value: string) {
  return value
    .replace(/\\/g, "\\\\")
    .replace(/\|/g, "\\|")
    .replace(/\r?\n/g, "<br />")
    .trim();
}

function parseStructuredReviewFinding(line: string): StructuredReviewFinding | null {
  const parts = line.split(/\s+\|\s+/).map((part) => part.trim());
  if (parts.length !== 5) {
    return null;
  }
  const [file, rawCategory, finding, recommendation, rawSeverity] = parts;
  if (!file || !finding || !recommendation || !/^category=/i.test(rawCategory)) {
    return null;
  }
  const category = rawCategory.replace(/^category=/i, "").trim();
  const severity = rawSeverity.replace(/^severity=/i, "").trim();
  if (!category || !severity) {
    return null;
  }
  if (!/^(critical|high|medium|low|info|warning|error)$/i.test(severity)) {
    return null;
  }
  return {
    file,
    category,
    finding,
    recommendation,
    severity,
  };
}

function buildStructuredReviewTable(rows: StructuredReviewFinding[]) {
  const header = [
    "| File | Category | Finding | Recommendation | Severity |",
    "| --- | --- | --- | --- | --- |",
  ];
  const body = rows.map(
    ({ file, category, finding, recommendation, severity }) =>
      `| \`${escapeTableCell(file)}\` | ${escapeTableCell(category)} | ${escapeTableCell(
        finding,
      )} | ${escapeTableCell(recommendation)} | ${escapeTableCell(severity)} |`,
  );
  return [...header, ...body].join("\n");
}

export function normalizeStructuredReviewTables(value: string) {
  const lines = value.split(/\r?\n/);
  let inFence = false;
  let pendingRows: StructuredReviewFinding[] = [];
  const output: string[] = [];

  const flushPendingRows = () => {
    if (pendingRows.length === 0) {
      return;
    }
    if (output.length > 0 && output[output.length - 1].trim()) {
      output.push("");
    }
    output.push(buildStructuredReviewTable(pendingRows));
    output.push("");
    pendingRows = [];
  };

  for (const line of lines) {
    const fenceMatch = line.match(/^\s*(```|~~~)/);
    if (fenceMatch) {
      flushPendingRows();
      inFence = !inFence;
      output.push(line);
      continue;
    }
    const structuredRow = inFence ? null : parseStructuredReviewFinding(line);
    if (structuredRow) {
      pendingRows.push(structuredRow);
      continue;
    }
    if (!inFence && pendingRows.length > 0 && !line.trim()) {
      continue;
    }
    flushPendingRows();
    output.push(line);
  }

  flushPendingRows();
  return output.join("\n");
}
