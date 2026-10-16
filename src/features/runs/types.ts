export type RunContext = { runId: string; instanceId: string };
export type SubmissionOutcome = "not_submitted" | "submitting" | "accepted" | "rejected" | "unknown";
export type ExecutionOutcome = "not_started" | "running" | "completed" | "failed" | "interrupted" | "unknown";
export type CaptureKind = "initial_turn" | "manual" | "recovered";
export type RunLaunchReceipt = RunContext & { outcome: SubmissionOutcome; threadId: string | null; turnId: string | null; message: string | null };
export type RunSettings = { modelId?: string | null; model?: string | null; modelLabel?: string; effort?: string | null; serviceTier?: string | null; collaborationMode?: Record<string, unknown> | null; accessMode?: string };
export type RunGitSnapshot = { available: boolean; baseOid?: string | null; headOid?: string | null; startedAt?: number; finishedAt?: number; concurrentChangeDetected?: boolean; atomic?: boolean; files: {path: string; status: string; diff: string; omission: string | null}[]; errors: string[] };
export type CaptureSummary = { id: string; kind: CaptureKind; turnId: string; label: string; createdAt: number; complete: boolean };
export type RunInstance = {
  id: string; runId: string; ordinal: number; requested: RunSettings; preparation: string;
  workspaceId: string | null; workspacePath: string | null; threadId: string | null; initialTurnId: string | null;
  submissionOutcome: SubmissionOutcome; executionOutcome: ExecutionOutcome; submission: Record<string, unknown> | null;
  baseline: RunGitSnapshot | null; setup: {status: string; message?: string}; error: string | null; captures: CaptureSummary[];
};
export type RunRecord = {
  schemaVersion: number; storeId: string; id: string; requestId: string; workspaceId: string; workspaceName: string; workspacePath: string;
  title: string; prompt: string; mode: "local" | "worktree"; createdAt: number; revision: number; baseOid: string | null;
  baseline: RunGitSnapshot; attachments: string[]; instances: RunInstance[];
};
export type RunCapture = CaptureSummary & {
  schemaVersion: number; instanceId: string; requestId: string; status: ExecutionOutcome;
  items: Record<string, unknown>[]; usage: Record<string, unknown> | null; observed: Record<string, unknown>; git: RunGitSnapshot; errors: string[];
};
export type RunComparison = {id: string; leftCaptureId: string; rightCaptureId: string; choice: "left" | "right" | "tie" | "undecided"; notes: string; revision: number; updatedAt: number};
export type CreateRunInput = { requestId: string; workspaceId: string; title: string; prompt: string; mode: "local" | "worktree"; images: string[]; instances: RunSettings[] };
export type RunPage = {storeId: string; runs: RunRecord[]; nextCursor: string | null};
export type SetupObservation = {status: "not_configured" | "already_dispatched" | "dispatched" | "dispatch_failed"; message?: string};
