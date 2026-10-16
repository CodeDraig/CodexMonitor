# Run records and comparison

Workspace Home records new local and worktree runs on the executing backend. A run has a persistent identity and one instance for every planned model selection, including instances that fail before creating a thread. Ordinary threads are not imported into run groups.

## Capture and compare

1. Start a run from Workspace Home. Preparation, submission, and execution are displayed independently.
2. Open the run comparison workbench and choose two saved captures. Captures from different runs in the same root workspace can be compared.
3. Inspect output, requested and submitted settings, command evidence, attributable token usage, and saved file patches.
4. Select a human preference and save comparison notes. If another client has changed the notes, reload the saved notes before saving again; a conflict preserves the current draft.
5. For a follow-up, expand the run, choose a later turn, and capture it explicitly.

The initial submitted turn is the primary result. Captures never change after creation. Recovery and manual captures have separate identities, so later conversations cannot silently change a saved comparison.

“Accepted” requires a turn ID or a correlated event on the registered initial thread. A transport failure or malformed reply remains unknown. Reconciliation reads history; it never launches another thread or turn. If a creation operation has an unresolved receipt, inspect the reserved workspace/thread information before starting a new run.

Setup status describes terminal dispatch, not process exit. Command evidence comes from app-server command items, including exit codes when supplied. Assistant text claiming that checks passed does not become command evidence. Token usage is unavailable when no turn-attributable observation was received; thread totals are not substituted.

## Git evidence

Recorded worktree batches resolve a single parent commit before creation. Each new branch starts at that commit, and an existing branch is never adopted for a tracked creation.

Saved Git evidence includes committed changes since the recorded base, staged and unstaged edits, and non-ignored untracked files. The pre-submission baseline is retained separately. Workspace evidence can contain setup, user, and other-process edits; turn-reported file changes are displayed separately.

Snapshots are observations, not atomic filesystem snapshots. They retain observation times and detected HEAD changes. Binary or oversized patches are marked explicitly. Text patches are limited to 2 MiB per file and 8 MiB per capture. A missing or non-Git workspace has unavailable Git evidence. Previously saved results and notes remain readable when the live workspace is removed.

## Storage and routing

The executing backend stores `runs.sqlite3` alongside `workspaces.json`. Local execution uses the app data directory; remote execution uses the daemon data directory. App and daemon share `src-tauri/src/shared/runs_core/` and `runs_rpc.rs`.

SQLite uses write-ahead logging (WAL), `synchronous=FULL`, and a five-second busy timeout. App and daemon may share one store on the same host and local filesystem. Existing stores switch to WAL without changing their schema version or identity. Opening an initialized store for a read does not acquire a writer transaction. Transactions protect run creation, operation reservations and receipts, captures, and comparison revisions across processes. Each recorded event and its instance transition commit together. Attachments remain blobs in the same database. Frontend state is isolated by backend and store identity. Unsupported schemas and database errors surface as errors.

The recorder retains a queued event across SQLite `BUSY` failures, retries in order after a short delay, and keeps the app-server reader free to deliver responses. Accepted responses and capture persistence also retry busy failures without repeating external submissions. Only SQLite busy errors retry; validation, I/O errors, and stale revisions remain errors. User-initiated comparison writes keep their `expectedRevision` contract: a busy failure is explicit, and a stale revision cannot overwrite a newer edit. The workbench retains an unsaved draft on save failure.

The pending recorder queue and an unsaved response are process memory, not a crash-proof spool. A process killed before a pending write commits still needs the existing history recovery path; recovery cannot manufacture missing token observations. A permanently held writer can keep recording pending and grow the queue. Committed records and acknowledged comparisons are checked after both writer processes exit in the concurrency acceptance test.

WAL stores must be shared on a local filesystem, not a network filesystem. Use SQLite's backup API for a live backup. Copying only `runs.sqlite3` while it is open can omit committed data still in `runs.sqlite3-wal`; do not discard its WAL or shared-memory sidecars during use. See [SQLite WAL documentation](https://www.sqlite.org/wal.html).

New runs require durable creation before external side effects. Existing untracked workspace/thread/message calls retain their original contracts. The optional `runContext` on tracked calls contains `runId` and `instanceId`.

Run commands use an `input` envelope on both the Tauri and daemon surfaces:

- `create_run`, `list_runs`, `get_run`, `update_run_metadata`
- `record_run_setup`, `record_run_failure`, `reconcile_run`
- `list_run_turns`, `capture_run_instance`, `get_run_capture`
- `save_run_comparison`, `list_run_comparisons`

`create_run` and manual capture request UUIDs are idempotent. Metadata and comparison writes require `expectedRevision`. Read the Rust contracts in `runs_core/types.rs` and the frontend contracts in `src/features/runs/types.ts` for payload fields.

The app-server reader queues recording work for registered threads. History uses paginated full-item reads, with a full `thread/read` fallback only when the server explicitly reports `list_turns is not supported yet`. A failed reconciliation read does not create a primary capture for an active turn. Database writes, paginated history reads, and Git capture execute outside the reader loop. The daemon continues recording when its client disconnects. When an app-server starts, it restores known run tracking and attempts history recovery. Unknown operation identities are never guessed from titles or prompt similarity.

## Validation

Focused tests live in `runs_core/store/tests.rs`, `runs_core/recording_tests.rs`, `runs_core/contention_tests.rs`, `runs_core/capture.rs`, the daemon RPC tests, and `src/features/runs/`. The recorder tests use a deterministic subprocess fixture, including events emitted before launch responses. Git tests use temporary repositories and verify HEAD and index preservation.

```sh
npm run typecheck
npm run test
cd src-tauri
cargo test --locked
cargo check --locked
```

Native desktop acceptance additionally requires launching a local run and a daemon-backed run, restarting or reconnecting, comparing and saving notes, and capturing a later turn. Mocked frontend tests and direct daemon RPC checks do not establish native desktop acceptance.

Run `python3 scripts/test-run-store-contention.py` from the repository root to exercise one store with separate desktop-library and daemon test executables. The default workload runs 500 evidence writes and 500 comparison edits per process over at least 30 seconds, after a forced 6.2-second writer lock. See [SQLite contention acceptance](run-store-contention.md) for assertions, results, and evidence boundaries.

Linux native results and retained evidence are documented in [Native run-record acceptance](native-run-acceptance.md).
