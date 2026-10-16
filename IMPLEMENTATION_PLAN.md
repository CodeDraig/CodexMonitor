# Durable Run Records and Run-Comparison Workbench

## Goal and decisions

Make run identity, launch receipts, and captured results durable on the executing backend, then build comparison views over those records.

- A run's primary result is its initial submitted turn. Later captures are separate records.
- Persistence uses SQLite through `rusqlite`.

## Current implementation basis

| Inspected behavior | Consequence for implementation |
| --- | --- |
| `src/features/workspaces/hooks/useWorkspaceHome.ts`, `useWorkspaceHome`, keeps run groups in React state. | Replace that state as the authoritative run store; retain local draft and selection state. |
| The launcher records instances after awaiting a send, and assigns `ready` when launching finishes. | Separate launch progress from execution outcome. "Launched" must not mean "completed." |
| `src/features/threads/hooks/useThreadMessaging.ts`, `sendMessageToThread`, extracts a turn ID but returns only `{status: "sent"}`. It can also return `blocked`. | Expose the launch receipt and handle blocked or uncertain submissions explicitly. |
| `src-tauri/src/shared/workspaces_core/worktree.rs`, `add_worktree_core`, can use an existing branch, a remote branch, or an implicit starting commit. | Recorded worktree batches need one resolved base commit and recorded creation receipts. |
| `src/features/app/hooks/useWorktreeSetupScript.ts` sends setup scripts to a terminal without awaiting their exit. | Record `dispatched`, not `succeeded`; preserve current setup behavior. |
| `src-tauri/src/shared/git_ui_core/diff.rs`, `get_git_diffs_inner`, compares the current HEAD with the working tree. | Add comparison against the recorded base so commits made during a run remain visible. |
| App and daemon have separate state objects and share backend cores; the managed daemon can share the desktop data directory. | Put persistence and recording in shared Rust code, with transactional cross-process updates. |

## 1. Define the durable record and result boundaries

Add shared Rust contracts and matching frontend types. Keep the existing `WorkspaceHomeRun` display shape as a temporary adapter while moving callers to the durable types.

The stored model contains these entities:

| Entity | Required information |
| --- | --- |
| `RunRecord` | Schema version, UUID, creation request ID, root workspace ID/name/path snapshot, title, original prompt, input attachments, mode, creation time, resolved batch base commit, planned instances. |
| `RunInstanceRecord` | UUID, run ID, ordinal, selected model ID/provider model/label, requested effort/service tier/collaboration/access settings, preparation phase, workspace and thread receipts, initial turn ID, submission outcome, execution outcome, errors. |
| `RunSubmission` | Exact expanded text and canonical input sent to `turn/start`; requested settings after existing frontend resolution; actual workspace path; observed runtime metadata with explicit unknowns. |
| `RunCapture` | UUID, instance ID, capture kind, capture time, source turn ID, terminal status, assistant output, command evidence, reported file changes, token evidence, Git snapshot, and completeness information. |
| `RunComparison` | UUID, left/right capture IDs, human choice (`left`, `right`, `tie`, `undecided`), notes, revision and update time. |

Use distinct state fields:

```ts
type SubmissionOutcome =
  | "not_submitted"
  | "submitting"
  | "accepted"
  | "rejected"
  | "unknown";

type ExecutionOutcome =
  | "not_started"
  | "running"
  | "completed"
  | "failed"
  | "interrupted"
  | "unknown";

type CaptureKind = "initial_turn" | "manual" | "recovered";
```

Required semantics:

- Allocate every planned instance before creating worktrees or threads. A failed instance remains identifiable.
- An accepted submission requires a recorded turn ID or equivalent unambiguous server evidence.
- A timeout after submission becomes `unknown`; it does not become a definite rejection.
- Captures are immutable. Later follow-ups, refreshed Git state, or recovered evidence create another capture.
- Keep requested settings separate from server-observed settings. A default model or service tier must not be presented as a known effective value.
- Preserve omitted versus explicit-null service-tier semantics already handled by the current messaging code.
- Capture command, exit code, output, and source item ID when available. An agent saying "tests passed" remains assistant text, not verified command evidence.

The first version covers runs launched through Workspace Home. Existing unrelated threads remain available through the normal thread UI; do not fabricate historical run groupings.

## 2. Add the shared SQLite store

Add `src-tauri/src/shared/runs_core.rs` with focused `store`, `types`, `recording`, and `capture` modules beneath `src-tauri/src/shared/runs_core/`. Add `src-tauri/src/shared/runs_rpc.rs`, following the existing contracts in `src-tauri/src/shared/workspace_rpc.rs`.

Store records in `runs.sqlite3` under the executing backend's data directory:

- Local execution uses the desktop app data directory.
- Remote execution uses the daemon's data directory.
- The frontend never opens the database directly.
- Store identity is a persistent UUID in database metadata. Frontend caches use `(storeId, runId)` to prevent remote-target switches from mixing records.

Use tables for runs, instances, operation receipts, captures, attachments, and comparisons. Keep searchable identifiers/statuses in columns and structured payloads in versioned JSON columns. Attachments can be stored as blobs initially, avoiding a second transaction boundary between the database and artifact files.

Required store behavior:

```text
create_run(input) -> RunRecord
get_run(run_id) -> RunRecord
list_runs(workspace_id, cursor, limit) -> RunPage

begin_operation(instance_id, phase, input) -> OperationDisposition
finish_operation(operation_id, receipt) -> RunInstanceRecord

append_capture(capture) -> RunCapture
save_comparison(input, expected_revision) -> RunComparison
```

`begin_operation` must transactionally return one of:

- permission to execute a new operation;
- the prior completed receipt;
- an unresolved operation requiring reconciliation.

This prevents a duplicate frontend request from automatically starting another instance.

Use short transactions and perform blocking database work outside the async executor's main work path. Do not hold database transactions while waiting for Codex, Git, or terminal operations.

SQLite provides the transaction mechanism needed here; `rusqlite` supports bundling SQLite rather than requiring a machine-wide installation. The exact dependency pin must pass the repository's compiler and target checks before landing. References: [SQLite atomic commit](https://www.sqlite.org/atomiccommit.html), [rusqlite bundled build documentation](https://github.com/rusqlite/rusqlite#usage).

Initialize a new schema without changing `settings.json` or `workspaces.json`. An unsupported schema version or corrupt database must produce a visible recording error, never an empty successful history.

## 3. Make the existing launch path record its side effects

Preserve the current frontend orchestration order:

1. Create the durable run and planned instances.
2. Create or select the instance workspace.
3. Dispatch the existing setup hook.
4. Create the thread.
5. Resolve and submit the initial message.

Add an optional tracking context through the existing calls:

```ts
type RunContext = {
  runId: string;
  instanceId: string;
};

// Additive options (interface sketches):
addWorktree(..., options?: { runContext?: RunContext });
startThread(workspaceId, options?: { runContext?: RunContext });
sendUserMessage(workspaceId, threadId, text, {
  ...existingOptions,
  runContext,
});
```

Calls without `runContext` retain their existing behavior and response contracts.

For tracked calls, the shared backend records operation intent before the external side effect and records the receipt before reporting success. App and daemon adapters call the same implementation.

Extend the frontend send result additively:

```ts
type RunLaunchReceipt = {
  runId: string;
  instanceId: string;
  outcome: "accepted" | "rejected" | "unknown";
  threadId: string | null;
  turnId: string | null;
  message: string | null;
};

type SendMessageResult = {
  status: "sent" | "blocked" | "steer_failed";
  turnId?: string;
  runReceipt?: RunLaunchReceipt;
};
```

The hook must inspect the receipt rather than treating a resolved promise as success. If the remote connection fails before a receipt returns, reload the instance record; do not resubmit automatically.

Concrete changes:

- `src/features/workspaces/hooks/useWorkspaceHome.ts`: create the record first, carry instance IDs through the loop, preserve failed entries, consume launch outcomes.
- `src/features/workspaces/hooks/useWorktreeOps.ts`: pass optional tracking context.
- `src/features/threads/hooks/useThreadActions.ts`: pass tracking context through thread creation.
- `src/features/threads/hooks/useThreadMessaging.ts`: retain prompt expansion and runtime preflight, expose receipts, and persist frontend validation failures for tracked instances.
- `src/features/app/hooks/useWorktreeSetupScript.ts`: return an observation such as `not_configured`, `already_dispatched`, `dispatched`, or `dispatch_failed`.
- Shared workspace and Codex cores: record the actual operation inputs and outputs.

For recorded worktree batches, resolve the parent HEAD once and create each new instance branch from that commit. Reserve the branch, destination path, and workspace ID before creation. A collision must fail visibly rather than adopt an unrelated existing branch. Ordinary untracked worktree creation keeps its current semantics.

Record both the batch base and the actual pre-submission workspace state. Copied instructions and setup scripts may change that state.

## 4. Capture results in the backend and reconcile interrupted recording

Add a shared recorder to `src-tauri/src/backend/app_server.rs`. Both desktop and daemon sessions receive the same recorder implementation.

Observe only registered run threads and their relevant turns. Record completed items, turn lifecycle events, attributable token usage, model reroutes, and launch responses. Register the instance/thread association before sending the initial turn so fast events cannot arrive before tracking exists.

The reader loop should enqueue ordered recording work. History requests and Git capture run outside that loop, avoiding a deadlock where the reader waits for a response it must itself consume.

When the initial turn reaches a terminal state:

1. Persist terminal evidence.
2. Assemble that turn's completed items.
3. Fetch missing history when necessary using the existing paginated history loader in `src-tauri/src/shared/codex_core/history.rs`.
4. Capture timestamped Git evidence.
5. Append the initial capture and publish its ID.

Do not build captures from `activeItems`, the sidebar's current thread, or the frontend scrollback window.

Recovery rules:

- On backend restart or explicit refresh, reconcile accepted or uncertain instances against available thread history.
- Match stored thread and turn identities. Do not infer identity from title or similar prompt text.
- If acceptance cannot be established, leave `unknown`.
- Never retry an uncertain `thread/start` or `turn/start` automatically.
- Missing workspaces or deleted threads leave the durable record and existing captures readable.
- A history-page failure yields incomplete evidence with an error, not an empty completed result.
- Later recovered evidence creates a new capture; it does not silently rewrite an earlier comparison.

Frontend disconnection must not stop daemon recording. Closing the local backend can interrupt execution; recovery reports what is actually recoverable.

## 5. Capture Git evidence with explicit provenance

Extend the shared Git diff implementation with a read-only operation against an explicit base commit:

```text
capture_run_git_state(
    workspace_id,
    base_oid,
) -> RunGitSnapshot
```

The snapshot contains:

- base commit and observed HEAD;
- observation start/end times;
- pre-existing dirty-state information;
- per-file status and text patches;
- binary/oversize/unavailable indicators;
- capture errors and detected concurrent changes.

Include commits made after the base, staged and unstaged changes, and non-ignored untracked files. Preserve whitespace in stored evidence regardless of the viewer's whitespace preference.

Store the patches and associated evidence so they remain readable after a worktree is removed. Reuse the current text/image limits where appropriate and mark omissions explicitly.

Display two separate sources:

- **Turn-reported changes:** attributable to recorded file-change items.
- **Workspace state at capture:** observed Git state, which may include setup, user, or other-process edits.

For dirty local runs, show the baseline dirty state alongside the captured result. Do not subtract patch text or claim that every difference was caused by the agent. A non-Git local workspace can still record a run, with Git evidence marked unavailable.

## 6. Expose run APIs and build the comparison UI

Add these commands through Tauri, the frontend IPC wrapper, and daemon RPC:

| Command | Input | Output |
| --- | --- | --- |
| `create_run` | Creation request UUID, workspace, mode, original input, planned instances/settings | Durable `RunRecord` |
| `list_runs` | Optional workspace filter, cursor, limit | `{storeId, runs, nextCursor}` |
| `get_run` | Run ID | Record, instance states, capture summaries |
| `update_run_metadata` | Run ID, expected revision, title | Updated record |
| `record_run_setup` | Instance ID, setup observation | Updated instance |
| `reconcile_run` | Run ID | Updated states and reconciliation errors; never relaunches |
| `capture_run_instance` | Instance ID, explicit turn ID, capture request UUID, label | New manual capture |
| `get_run_capture` | Capture ID | Immutable capture detail |
| `save_run_comparison` | Capture pair, choice, notes, expected revision | Saved comparison |
| `list_run_comparisons` | Run ID or capture ID | Saved comparisons |

Repeated creation/capture request UUIDs return the existing result. Revision conflicts return a conflict result without overwriting another client's notes.

Implement adapters in the existing command registry and daemon dispatcher. Keep all frontend calls in `src/services/tauri.ts`. Fetch on entry, focus, and reconnect; poll while a visible run is active. This first version does not need another event protocol.

Add a `runs` frontend feature containing a record-loading hook, run detail view, comparison view, and capture selectors. Wire it through `src/features/app/hooks/useMainAppComposerWorkspaceState.ts` and the current Workspace Home composition.

The UI provides:

- Durable run cards with preparation, submission, and execution status.
- Two selected captures shown side by side, or stacked on narrow layouts.
- Output, settings, command evidence, token evidence, and changed-file tabs.
- Base-commit, input, settings, dirty-state, and completeness differences.
- Saved human choice and notes.
- Links to surviving threads and workspaces.
- An explicit action to capture a later turn.

Reuse `src/features/git/components/GitDiffViewer.tsx` in read-only mode and existing design-system primitives. Comparing instances within one run comes first; selecting captures from another run in the same workspace uses the same interface.

Store only attributable token observations. If turn usage is unavailable, show "unavailable"; do not substitute current thread totals or invented cost estimates.

## Acceptance tests

These are implementation requirements. They have not been executed as part of planning.

| Test | Concrete acceptance |
| --- | --- |
| **Durable identity** | Create a two-instance run, reopen the store, and recover identical IDs, input, settings, instance order, and attachment bytes. Repeating the creation request produces one run. |
| **Cross-process writes** | Two store connections update different instances and save comparison notes. No update is lost; a stale notes revision returns a conflict. |
| **Recorded base** | Resolve base commit A, advance the parent to B before the second instance is created, and verify both recorded worktrees start at A. A branch collision creates no adoption or reset. |
| **Submission outcomes** | Fixtures return accepted, explicit rejection, malformed success, and timeout-after-write. Only accepted evidence records an accepted turn; ambiguity remains unknown. |
| **Retry behavior** | Repeat a tracked operation after losing its response. A completed receipt is reused; an unresolved operation does not execute again. |
| **Partial launch** | First instance launches; second fails during preparation. Restart and verify both entries, the first receipt, and the second failure remain visible. |
| **Initial-turn isolation** | Complete turn T1, then submit T2. The primary capture and its comparison remain unchanged. Capturing T2 creates a separate selectable record. |
| **Frontend independence** | Launch through the daemon, disconnect the client, complete T1, and reconnect. The capture is available without opening the thread first. |
| **History recovery** | Recover a known initial turn from multiple pages. Duplicate anchors are reconciled; failure of a required page remains visible as incomplete evidence. |
| **Git capture** | A fixture repository contains a pre-existing edit, a run-created commit, staged/unstaged edits, an untracked file, and a binary file. The result retains the baseline, includes post-base commits, and marks binary limitations. Reading it changes neither HEAD nor index. |
| **Setup and evidence labels** | Terminal dispatch without an exit receipt displays `dispatched`. Assistant text saying "tests passed" does not create a successful check. |
| **Comparison persistence** | Save a capture pair and notes, restart, then remove an instance workspace. Captured output/diffs and notes remain readable; the missing live workspace link is identified. |
| **Backend isolation and parity** | The same request fixtures pass through local adapters and daemon dispatch. Switching between two stores with overlapping workspace IDs does not mix displayed runs. |
| **UI behavior** | Loading, partial capture, unknown submission, failed instance, missing workspace, notes conflict, and narrow-layout comparison are rendered and navigable. |

Place tests beside the new shared modules and frontend feature, and extend existing Workspace Home, messaging, IPC, and daemon dispatcher tests. Use temporary repositories and a fake app-server for automated boundary tests.

After focused tests pass, run:

```bash
npm run typecheck
npm run test
cd src-tauri
cargo test --locked
cargo check --locked
```

Then perform actual desktop acceptance for a local run and a daemon-backed run: launch, restart/reconnect, compare, annotate, and capture a later turn. Platform-specific acceptance must be reported separately from Linux or mocked results.

## Implementation order and boundary

Implement in this order: **store and contracts -> tracked launch receipts -> backend capture/recovery -> Git evidence -> comparison UI -> integrated acceptance**. Update canonical documentation with the delivered behavior.

The scope includes durable records and human comparison. Automated judging, automatic merge/application of a winner, background relaunch, and a general benchmark scheduler are separate features. Setup-script completion tracking is also separate; this implementation records the current dispatch semantics accurately.

The main remaining technical uncertainty is which historical payloads retain turn-attributable usage and model information. Resolve that with captured protocol fixtures during the recording slice, and represent unavailable fields explicitly. SQLite target compatibility still needs build verification.

The next sharp edge is the boundary between a persisted launch intent and an external operation whose response is lost; implement the receipt and recovery tests before the UI.
