# SQLite contention acceptance

The shared run store uses WAL with full synchronous durability. Initialized-store reads do not reserve SQLite's writer lock. The recorder saves an event and its instance transition atomically and retains pending events, server responses, and captures across SQLite busy timeouts. App and daemon use the same implementation; no IPC payload or schema version changed.

## Reproduced failure mechanisms

Two deterministic tests failed against the preceding store implementation:

- Holding a read transaction prevented an evidence write from committing under rollback journaling.
- Holding a write transaction prevented a second connection from opening the initialized store for reading, because every open used `BEGIN IMMEDIATE`.

The prior recorder logged and discarded an event if its store call exhausted the five-second busy timeout. A busy failure while saving an already received server response also returned without retaining that receipt. Retrying an external request would risk a duplicate run; the implementation instead retries persistence of the original response. These mechanisms explain contention sensitivity. The native log alone does not identify which connection held its particular lock.

WAL removes reader-versus-writer commit blocking. It still permits only one writer at a time, so the five-second timeout remains and recorder persistence retries errors identified from SQLite's `SQLITE_BUSY` code. Non-busy errors remain errors. See [SQLite WAL](https://www.sqlite.org/wal.html) and [busy-timeout semantics](https://www.sqlite.org/c3ref/busy_timeout.html).

## Sustained cross-process check

From the repository root:

```sh
python3 scripts/test-run-store-contention.py
# Adjust only workload size and pacing:
python3 scripts/test-run-store-contention.py --iterations 500 --seconds 30
```

The script compiles the desktop library and daemon test targets, discovers their executable paths from Cargo's JSON output, and runs two separate OS processes against one isolated database. Both processes exercise the production shared recorder and run RPC dispatcher. The test does not launch a Tauri window or use the daemon's TCP transport. Existing native GUI acceptance is documented separately in [Native run-record acceptance](native-run-acceptance.md).

Each process writes 500 distinct 2 KiB output items and a terminal event, persists a final capture, and submits 500 competing edits to the same comparison. Comparison edits append unique operation identifiers to notes using `expectedRevision`. A test client explicitly reloads and reapplies its pending edit after a reported conflict; the product does not silently merge notes. The test records the revision returned by every successful save.

The parent deliberately holds `BEGIN IMMEDIATE` for 6.2 seconds, exceeding the production timeout. A separate read transaction remains open throughout the write workload, preventing checkpoint completion while writers continue. Both clients also poll the run detail and run list. Each write lane is paced for at least 30 seconds after the forced contention. The smaller ordinary Rust-suite variant uses 120 iterations and 10 seconds per lane; without the script's executable override, both subprocesses use the current test target.

Assertions verify:

- Every evidence item has its exact expected ID and payload; the terminal event and completed instance state survive.
- Both final captures contain every output item, and both instance capture lists retain the original and final capture.
- The original comparison capture contents remain unchanged.
- Every acknowledged comparison revision appears exactly once, in sequence, with its corresponding operation identifier in the final notes. No edit is silently overwritten.
- Both processes actually encounter a busy timeout.
- Store identity, all evidence, captures, and notes remain readable after both processes exit and the store is reopened.
- `PRAGMA integrity_check` returns `ok` and `pragma_foreign_key_check` returns no violations.

The verified mixed-executable run on 2026-10-16 completed in 39.70 seconds:

| Observation | Desktop | Daemon | Total |
| --- | ---: | ---: | ---: |
| Evidence events retained | 501 | 501 | 1,002 |
| Output items in final captures | 500 | 500 | 1,000 |
| Acknowledged comparison edits | 500 | 500 | 1,000 |
| Read polls | 1,000 | 1,000 | 2,000 |
| Explicit comparison busy failures, retried by test client | 1 | 1 | 2 |
| Explicit stale revisions, reapplied by test client | 3 | 1 | 4 |

The held reader accumulated 43,062,272 bytes of WAL while commits continued. All loss, identity, capture, and integrity assertions passed. The test prints its fixture directory and writes per-process logs, acknowledgment receipts, and `result.json` there.

## Focused regressions

`src-tauri/src/shared/runs_core/store/tests.rs` checks reader/writer independence, existing rollback-journal conversion preserving records and store identity, full synchronous mode, error-code classification, and rollback of evidence when an injected instance update fails.

`src-tauri/src/shared/runs_core/recording_tests.rs` uses the actual app-server stdout reader with a deterministic subprocess. Locks lasting 6.2 seconds check queued-event retention, accepted-response persistence without a second external submission, and capture persistence retaining its request identity. The stdout reader must still deliver the server response while the recorder is waiting.

## Verification and changed files

- `python3 scripts/test-run-store-contention.py`: passed with distinct desktop and daemon test executables, 500 iterations per process, and 30-second pacing.
- `cargo test --locked --manifest-path src-tauri/Cargo.toml`: passed, 482 tests (250 library, 203 daemon, 28 daemonctl, 1 configuration).
- `cargo check --locked --manifest-path src-tauri/Cargo.toml`: passed with existing repository warnings.
- `npm run typecheck`: passed.
- `git diff --check` and whitespace checks on the new files: passed.
- The full frontend suite was not repeated because this change does not alter frontend code.
- `IMPLEMENTATION_PLAN.md` remains unchanged: SHA-256 `a21b70485abcffb966f5375db31087bafb90c28db1fdde969967442d23775250`.

The two baseline lock regression failures are intentional reproductions. The first stress-test compile failed because Tokio's test/join macros are not enabled in this repository. The test now uses the existing runtime builder and `futures_util::future::join`; no dependency or feature change was added. All final checks pass.

Implementation and test files changed for this repair:

- `src-tauri/src/shared/runs_core/store.rs`: WAL initialization, read-only open path, SQLite busy classification, and test-only raw evidence helper.
- `src-tauri/src/shared/runs_core/store/events.rs`: transactional event/instance persistence.
- `src-tauri/src/shared/runs_core/recording.rs`: retention and retry of events, receipts, and captures.
- `src-tauri/src/shared/runs_core/store/tests.rs`: locking, rollback, compatibility, and error-classification checks.
- `src-tauri/src/shared/runs_core/recording_tests.rs` and `test_app_server.py`: queued-event, receipt, and capture regressions.
- `src-tauri/src/shared/runs_core/contention_tests.rs` and `src-tauri/src/shared/runs_core.rs`: sustained process workload and test registration.
- `scripts/test-run-store-contention.py`: discover and run the desktop and daemon test executables together.
- `docs/run-records.md`, `docs/native-run-acceptance.md`, and this report: current storage contract, result references, and evidence limits.

## Evidence and limits

Local evidence is retained in `.agent/sqlite-contention-20261016/` (Git-ignored): baseline failures, focused and full Rust test logs, mixed-process stress log, typecheck/check logs, pre-edit source backups, `mixed-process-evidence/` with worker acknowledgment receipts, and a consistent SQLite backup `verified-runs.sqlite3`. Original temporary fixtures remain available; no live application store was used or modified.

WAL sharing assumes one host and a local filesystem. These checks establish retention across live contention and process exit after committed writes. They do not establish power-loss behavior, a crash-proof pending-event spool, network-filesystem support, or packaged release acceptance. The repaired build also passes the separate [native local/daemon acceptance](native-run-acceptance.md), including concurrent access to the same store and restoration of evidence and notes after restarting both backends. A process killed before its pending evidence commits may still lose observations unavailable from history; permanent lock ownership can leave its recorder pending. Those are the next recovery and resource-boundary edges.
