# Native ACP conversation acceptance

The Linux development build integrates ACP v1 conversations with the ordinary sidebar, main message view, composer, recorded runs, and comparisons. OpenCode is the real-agent acceptance target. An independent generic ACP fixture exercises the same native UI and shared protocol implementation. This establishes the behavior below on Linux; it does not establish packaged-release, Windows, macOS, or arbitrary-agent acceptance.

## Environment and isolation

- Fedora 43, x86-64, X11, native Tauri/WebKitGTK windows.
- OpenCode `/home/cadwaladyr/.opencode/bin/opencode`, version `2.0.30`, ACP protocol version 1, model `opencode/big-pickle`.
- Generic profile: `python3 src-tauri/src/shared/agent_runtime/fixtures/conversations_acp.py --catalog <isolated catalog>`. Its argument parser rejects an automatically appended `acp` argument. Its catalog is test-owned fixture data, not a production agent database reader.
- Embedded frontend assets: `npm run build`, then `cargo build --locked --features custom-protocol --manifest-path src-tauri/Cargo.toml --bin codex-monitor --bin codex_monitor_daemon`.
- Native inspection/control: PID-scoped AT-SPI through `pyatspi`, `xdotool`, and ImageMagick. The computer-use tool cannot bind this native window in this environment; browser-only tests do not substitute for these checks.
- Scratch repositories: `/tmp/monitor-acp-conversations-1mm9vgnu/{local,remote,generic,generic-remote}`. Local and remote repositories have isolated Git histories and agent configuration.
- Test data/evidence: `.agent/acp-conversations-20261017/native/`. Both OpenCode and Monitor use isolated `XDG_DATA_HOME` directories. The local desktop and daemon share `native/local-data/com.dimillian.codexmonitor/runs.sqlite3`; the remote desktop has separate connection settings.
- Authenticated daemon readiness and listener PID are checked before opening clients. Test token/configuration are fixture values, not installation defaults.

The installed desktop/daemon, normal user data, historical plans, and existing acceptance evidence remain unchanged. Baseline and final SHA-256 receipts are in `.agent/acp-conversations-20261017/preservation-*.txt`. The task-owned native processes are stopped after acceptance; their data, SQLite snapshots, scripts, and screenshots remain available.

## Verified native behavior

ACP configuration control acceptance is recorded in `.agent/acp-controls-20261017/native/acceptance.json`. Native local and daemon-backed OpenCode 2.0.30 windows open model, effort, and mode menus with physical mouse input and apply keyboard-selected values through ACP. The native UI and durable session state agree on High effort, Plan mode, and the selected Big Pickle model; options update when the model changes. The attachment action is explicitly optional and the bare file picker is hidden. No prompt is submitted in these isolated selector checks. The desktop SHA-256 is `388b61b121c7c1b533be3b349cbede87fd122e49b868bd2070c26dd704d9cc5c`; the daemon remains `4b31e58161527e57a13bc1074de8ac96ddcf967119176292fbd6a8d23b945a01`. The native long-open-menu check is interrupted by desktop focus returning to Codex; regression tests verify that automatic replay waits while an ACP control has focus and explicit refresh still runs. This host limitation does not establish a passed 35-second visual hold.

The sidebar creation chooser is verified in `.agent/sidebar-agent-picker-20261017/native/acceptance.json`. Both local and daemon-backed native windows create OpenCode 2.0.30 and independent generic ACP conversations through **workspace + → New agent**; their normal composers identify the chosen profile. Choosing a new profile while viewing another ACP conversation respects the explicit choice. In thread-list mode, **New thread in project → project** opens the same chooser. Choosing **Codex** opens the normal empty Codex draft and creates no extra ACP binding. These creation checks submit no prompts. The final development desktop SHA-256 is `a063780b0af24bb00f67fbc51e7a20eb342084bb34c8c95faad593b521535402`; its matching daemon is `4b31e58161527e57a13bc1074de8ac96ddcf967119176292fbd6a8d23b945a01`. The task-owned windows and daemon are stopped, and isolated data and diagnostic attempts remain available.

| Check | Result and evidence |
| --- | --- |
| External OpenCode discovery | Two-turn conversations created through the standalone OpenCode CLI are returned by public global ACP discovery with their external repository directories. The global browser offers **Add workspace and open**. |
| Workspace registration | Imported directories appear as normal workspaces without starting Codex. Local and daemon ACP imports share current persisted state; stale process maps do not replace the other backend's registrations. Process-tree evidence contains no Codex agent. |
| Complete supplied history | The normal main view shows the externally created turns, Monitor continuations, and later external updates. Projection verification asserts exactly one assistant result for each expected marker after repeated replay and restart. |
| Open by ID | The independent generic session is opened through the native browser form. Initialization confirms load support; catalog and Monitor session identities are reused without duplicates. Unsupported loading is rejected by shared contract tests. |
| Normal composer | Both native desktops submit OpenCode follow-ups through **Message OpenCode**. Independent generic ACP conversations use **Message Independent ACP** through the same main composer and store contracts. |
| External idle update | `ACP_EXTERNAL_LOCAL_IDLE`, created outside Monitor through the CLI, appears through automatic idle replay without manual refresh. Accessibility and store snapshots retain the observation. |
| Shared ACP rendering | The independent fixture renders repeated messages, a patched historical tool, plans, compaction limits, an embedded text resource, and a lazily loaded image. Local and daemon continuations and reopened cached history are verified. |
| Original permission choices | A daemon-backed OpenCode tool request appears in the normal main view with **Allow once**, **Always allow**, and **Reject**. Choosing **Allow once** executes `printf ACP_NATIVE_CAPTURE_TOOL` and completes the recorded initial result. |
| Concurrent writes | Local and daemon native prompts complete against the shared store while comparison notes advance to revision 2. A ten-second writer lock overlaps submission; both prompts have exactly one durable request/execution and their final answers persist. The separate sustained test exercises writes blocked beyond the five-second busy timeout. |
| Immutable and later evidence | The complete initial capture and separately requested follow-up capture remain distinct. Their payload hashes are unchanged across replay, concurrent submissions, and desktop/daemon restarts. |
| Comparison notes | Both native workbenches reopen the same saved pair and display `ACP_FINAL_CONCURRENT_NOTES`. The database retains both acknowledged revisions. |
| Presentation and ownership | A pinned Monitor title override survives restart. Selecting a daemon-owned conversation locally displays its cached history with a read-only ownership label; it does not steal execution ownership. |
| Recorded-run links | The recorded initial-execution link opens the corresponding ordinary main conversation and ACP composer. Repeating the same link after returning Home also works. |
| Backend status | The remote indicator reflects ACP readiness; an ACP connection does not require a connected Codex workspace. |
| Restart and integrity | Before-restart journal, run, capture, attachment, operation, and comparison-revision rows remain present byte-for-byte. Both capture hashes match. SQLite integrity is `ok`, foreign-key checks are empty, and successful snapshot pointers reference completed load generations. |

The real-agent native checks use only the public ACP protocol and Monitor APIs. They do not inspect OpenCode's database or use a provider-specific REST history fallback. An early CLI fixture inherited the launcher's `PWD`; the retained discovery receipts identify those sessions separately. Accepted external sessions have verified scratch-directory paths, and ACP subprocesses receive a matching `PWD` and process working directory.

## Record identities and retained receipts

The native shared store is `57a2fbc3-b836-4c9e-870d-79fad941777d`.

| Record | Monitor ID |
| --- | --- |
| Local external OpenCode session | `8cfbc859-0025-4525-959a-41bcfff1f970` |
| Daemon external OpenCode session | `13082452-618d-4ed9-a0d4-7498ad64adcd` |
| Recorded run | `feec4980-56ae-4c71-a1a6-cb7ffb35fbc7` |
| Initial capture | `5e8cc213-cb00-4e28-9832-8d6b58f868b7` |
| Separate follow-up capture | `9e5027a2-c8d0-46c2-82ab-eecbcb8a17ab` |
| Saved comparison | `b688679a-ed92-4edb-af32-64b20013add5` |

Evidence below is relative to `.agent/acp-conversations-20261017/native/`:

- `external.py`, `*-external.jsonl`, verified native-ID files, discovery snapshots: external creation and protocol-only discovery.
- `native.py`, `rpc.py`, `start.py`, `restart.py`, `verify.py`: reproducible PID-scoped native control, authenticated RPC, consistent snapshots, and preservation checks.
- `local-idle-success-ui.json`: idle refresh of externally written history.
- `initial-capture-evidence.json`, `later-capture.json`, `native-concurrent-receipts.json`: complete captures and unique terminal native submissions.
- `native-lock-acquired.json`, `native-lock-released.json`: writer-lock timing.
- `native-metadata-before-restart.json`, `local-readonly-remote-ui.json`, `native-processes.json`: presentation metadata, read-only ownership, and no Codex startup.
- `final-conversation-projection-verification.json`: exactly-once expected assistant responses in the current materialized generations.
- `final-restart-verification.json`, `final-native-ui-verification.json`: immutable evidence, metadata, workspaces, comparison revisions, integrity, and both native note editors.
- `delivery-native-verification.json`: native Open-by-ID identity reuse. `presentation-native-verification.json`, `presentation-opencode-replays.json`, and `presentation-restart-verification.json`: final embedded executable hashes, committed real OpenCode replay on both backends, immutable captures, saved metadata, and both note editors.
- `presentation-opencode-local.png`, `presentation-opencode-daemon.png`, `presentation-compaction-fallback.png`, `delivery-open-by-id-main.png`, `final-local-notes.png`, `final-daemon-notes.png`: native rendering, including inspectable plain-text compaction fallback.
- `before-final-restart.sqlite3`, `final-native.sqlite3`, `delivery-restarted.sqlite3`, `presentation-final.sqlite3`: SQLite-consistent database snapshots.

Existing evidence under `.agent/multi-runtime-20261016/` is preserved separately. Real Codex native execution is not rerun as part of this ACP-conversation acceptance; Codex regression behavior is exercised by the current automated suites.

## Automated acceptance

```sh
npm run typecheck
npm run test
npm run build
cargo check --locked --all-targets --manifest-path src-tauri/Cargo.toml
cargo test --locked --features custom-protocol --manifest-path src-tauri/Cargo.toml
python3 scripts/test-runtime-store-contention.py
```

The frontend suite passes 1,067 tests in 152 files. The full Rust suite passes 543 tests across the desktop library, daemon, control tool, and configuration integration test. The full Rust suite and the sustained runner are executed separately; the two mixed-runtime sustained entry points are opt-in.

The frontend suite covers conversation identity/projection, idle refresh with retained drafts, queueing, backend isolation, repeated recorded-run navigation, targeted sidebar refresh, no Codex startup during ACP workspace restoration, generic and legacy ACP recorded runs, and runtime-owned Codex event coalescing. Existing Codex tests remain part of the full suite.

The shared ACP conformance tests run against OpenCode-labelled and independently labelled generic fixtures with exact differing launch arguments. They cover object capability markers, independent source pagination and failures, native-ID collisions, missing directories, stale cursors, limited discovery/loading, repeated identical messages, optional IDs, chunk order, tool patches, compaction, child-session routing, replay barriers, post-response live events, failed-refresh retention, load idempotency while a later live permission is pending, cached read-only ownership, metadata revisions, and unsupported existing-session loading.

The schema-2 migration fixture contains a run, operation, evidence, execution, immutable initial and manual captures, binary attachment, comparison notes, and comparison revisions. It compares original table payloads and attachment bytes before and after migration and checks a consistent schema-2 recovery backup. Legacy ambiguous bindings remain aliases with their original Monitor UUIDs and payloads.

The sustained runner launches separate desktop-library and daemon test executables for at least 62 seconds. It mixes live Codex/ACP execution, captures, repeated asynchronous replay generations, duplicate request IDs, workspace imports from stale backend maps, and revision-checked comparison writes. It holds a writer lock for 6.5 seconds, exceeding the five-second busy timeout, while a WAL reader remains open. The passing retained workload at `/tmp/monitor-runtime-71873578-8939-4ade-8c4c-1e45f53f5645/` contains:

- 738 executions and 5,513 durable runtime events;
- 736 completed history loads and two published conversation snapshots;
- four captures and 736 recoverable comparison revisions;
- both imported directories and successful integrity/foreign-key checks.

A contention check exposed a generation-pointer/load-status read race. Conversation reads now use one deferred SQLite read transaction, preserving writer-free reads while preventing mixed publication state. A full-suite run overlapping the sustained workload exceeded existing timing deadlines; isolated verification preserves those deadlines and runs the full suite and sustained runner separately. Logs, including failed diagnostic attempts, are retained under `.agent/acp-conversations-20261017/`.

## Boundaries and reproduction

For a fresh native run, create isolated repositories and data directories, configure OpenCode and the independent fixture on the executing backends, verify the daemon listener, and open local and remote native windows. Create external CLI turns with the intended directory and matching `PWD`; discover and import them through the browser, inspect and continue them in the normal main view, add an external update while idle, and exercise original permission choices. Select captures by their verified execution IDs, edit comparison notes during concurrent activity, save consistent before/after snapshots, and restart both desktop and daemon before checking history and hashes. Keep the test data and recovery backups.

Complete history means the conversation supplied by a successful agent replay. Compacted originals or omitted content cannot be reconstructed. Failed refreshes retain the last successful snapshot. Historical permissions cannot authorize tools or create execution receipts, and historical turns are not promoted into run captures.

Filesystem and terminal client callbacks are not advertised. Supported prompt/media/configuration behavior follows negotiated capabilities; unknown supplied content remains inspectable. Pending protocol queues are process memory rather than a crash-proof disk spool. Owner death can leave execution explicitly uncertain; Monitor never resubmits it automatically.

The generic fixture establishes shared ACP interoperability, not real-agent acceptance for Claude Code, Antigravity, every OpenCode provider/version, or other ACP launchers. V2 migration and native Windows/macOS/package acceptance remain separate work. Existing Rust platform/dead-code warnings and Vite's large-chunk warning remain visible in build logs.
