# Native run-record acceptance

The repaired SQLite build passes Linux native desktop acceptance on 2026-10-16 for local and daemon-backed runs, concurrent access to one store, and restart persistence. The checks use the actual debug Tauri/WebKitGTK application and TCP daemon. They do not establish macOS, Windows, or packaged-release acceptance.

## Native inspection and isolation

Open Computer Use returned `appNotFound("Codex Monitor")` while the repaired application was running. Native inspection used Linux AT-SPI, keyboard and pointer events through `xdotool`, and X11 window screenshots through ImageMagick. No browser mock or replacement frontend was used.

Two native desktop processes ran simultaneously. The first used local execution with an isolated `XDG_DATA_HOME`. The second had a separate frontend data directory and connected to the daemon on `127.0.0.1:54733`. The daemon's data directory was exactly the first desktop's app-data directory, so the desktop backend and daemon wrote the same `runs.sqlite3`. Both exposed store UUID `83d45cb1-c01f-4a8e-a059-64e964c36d55`.

The scratch workspaces were `/tmp/codexmonitor-repaired-local-20261016` and `/tmp/codexmonitor-repaired-daemon-20261016`. The normal Codex configuration and authentication were used without modification. No normal application data directory or user workspace registration was changed.

## Native results

| Check | Verified result |
| --- | --- |
| Local native launch | Workspace Home submitted the run through the native Tauri bridge. Its initial capture contains `LOCAL_NATIVE_REPAIRED`, eight separately numbered command outputs with exit code 0, and the saved `evidence-local.txt` patch. |
| Concurrent local and daemon runs | Both recorded initial turns were running in the same store when a fixture connection acquired `BEGIN IMMEDIATE` for seven seconds. Both real backends logged `Run recording waiting for busy SQLite store; event retained`. Both completed with full initial captures. |
| Daemon native launch and client independence | A verified complete prompt was submitted from the remote desktop. That desktop was stopped while the turn was running. The daemon completed `sleep 15`, wrote and read the evidence file, and saved `DAEMON_NATIVE_REPAIRED` without its client. |
| Reconnect | The reopened remote desktop displayed the saved daemon result in the comparison workbench before Open thread was used. |
| Later captures | Native follow-ups returned `LOCAL_LATER_REPAIRED` and `DAEMON_LATER_REPAIRED`. Choose a later turn and Capture later turn saved them as separate manual captures. |
| Evidence display | Native Output, Commands, Tokens, Settings, and Files views were inspected. The saved daemon file patch was visible. Initial captures have attributable token observations; manual captures without such observations report unavailability. |
| Comparison conflict | The actual daemon TCP API saved ten note edits while the desktop held an older revision. A native save showed the stale-revision error and retained `LOCAL_DRAFT_RETAINED_ON_STALE_REVISION`. Reloading and merging in the desktop saved revision 12. |
| Sustained comparison writes | The actual daemon saved 100 uniquely identified edits, paced at 300 ms with a pause for the native conflict-resolution step. Its acknowledgment revisions are exactly 2–11 and 13–102. Native revisions 1 and 12 retained the base and merged note. Final notes contain every daemon identifier once and the desktop's merged marker. |
| Native busy save | An eight-second fixture writer lock made Save comparison report `Run store busy: database is locked`. The full draft, including all concurrent edits, remained in the textarea. Retrying after lock release saved revision 103. |
| Full restart | Both desktops, the daemon, and their owned child processes were stopped. The repaired binaries were restarted. Both native workbenches restored the exact notes and `Tie` choices; the local comparison remained at revision 103 and the daemon comparison at revision 1. |
| Evidence and integrity after restart | All five capture payloads were byte-for-byte unchanged. Exact row comparisons passed for all eight store tables, including all 37 evidence rows, six operation receipts, instance states, comparison payloads, and store identity. SQLite integrity returned `ok`; foreign-key checks returned no violations. |

The three initial captures contain 28 completed items. Every recorded completed item was matched against its saved capture, including IDs, command text, exit codes, output, and assistant text. The five initial/manual captures contain 34 items in total. The initial run captured from the truncated-prompt fixture is included in those counts and remains intact.

## Harness corrections and evidence boundaries

The first remote prompt was sent before all queued keyboard events reached the native field. Its submitted text was truncated. That run and its eight completed sleep commands remain preserved. The full daemon launch was repeated after checking the native field contained the complete prompt.

An initial comparison attempt read the database immediately after an asynchronous native save, and subsequent harness commands continued after the assertion failed. That sequence intentionally replaced its notes and is not counted as the successful concurrent-notes check. Its comparison remains preserved at revision 13 with the harness error receipt. The accepted check used a separate ordered capture pair and waited for acknowledged saves before advancing. Its 100 daemon acknowledgments, native saves, conflict handling, busy retry, and restart restoration all passed.

Expected injected busy and revision-conflict errors were observed. The application and daemon logs contain the retained-event notices, with no recorder failure during the accepted run. Existing EGL and Codex configuration warnings were left unchanged.

The locks were released normally. These checks do not establish survival of a process killed before a pending write commits, power-loss durability, or a crash-proof event spool. WAL sharing assumes one host and a local filesystem. The focused and sustained backend tests are documented in [SQLite contention acceptance](run-store-contention.md).

## Build and verification

- `cargo build --locked --manifest-path src-tauri/Cargo.toml --bin codex-monitor --bin codex_monitor_daemon`: passed.
- `npm run typecheck`: passed.
- Native local/daemon launches, forced-lock experiments, concurrent daemon TCP note writes, UI conflict/busy handling, and full restart checks: passed as described above.
- `git diff --check`: passed.
- Application source hashes are unchanged throughout this acceptance pass. No additional product-code repair was needed.
- Rust and frontend suites were not repeated in this native-only pass; the preceding repaired-store verification passed 482 Rust tests.
- `IMPLEMENTATION_PLAN.md` remains unchanged, SHA-256 `a21b70485abcffb966f5375db31087bafb90c28db1fdde969967442d23775250`.

The running process executables were hashed through `/proc/<pid>/exe` and matched the built files:

| Executable | SHA-256 |
| --- | --- |
| Desktop, both instances | `fc6dc6b41a1e2cf0d01fdc23478b6f8b967040616306c3e1b886fa25863b0e9c` |
| Daemon | `aaff0a892d36f69b8c09aee089c5eece26340304c4c95f9e300e9e7b544881e7` |

## Evidence and recoverability

Evidence is retained locally in `.agent/native-repaired-20261016/` (Git-ignored):

- `acceptance-result.json`, `executables.json`: checks, counts, binary identities, and limitations.
- `before-restart.json`, `after-restart.json`, `restart-table-checks.json`: capture hashes, evidence counts, instance identities, comparison payloads, and exact equality checks for all eight store tables.
- `local-restored-comparison.png`, `remote-restored-comparison.png`: inspected native views after the full restart.
- `local-restart-restored.txt`, `daemon-restart-restored.txt`, `*-native-restored-notes.txt`: accessibility snapshots and exact restored textarea contents.
- `concurrent-observation.json`, `lock-acquired.json`: simultaneous native run state and seven-second writer-lock observations.
- `notes-retry/notes-writer-receipts.json`: all 100 daemon comparison acknowledgments.
- `verified-conflict.txt`, `local-busy-error.txt`, `busy-draft.txt`, `local-after-busy-ack.json`: native error handling and successful retry.
- `accepted-runs.sqlite3`, `before-restart.sqlite3`: consistent SQLite backup copies.
- `notes-harness-error.json`, the original comparison receipts, and original prompt/run records: preserved unsuccessful harness attempts.
- Native control scripts, build/typecheck logs, process logs, source hashes, and pre-update documentation copies.

All task-owned desktops, daemon, Vite server, and child processes are stopped. Ports 1420 and 54733 are no longer listening. Scratch workspaces, data directories, and backups are retained. Earlier acceptance evidence remains untouched in `.agent/native-run-acceptance-20261016/`.
